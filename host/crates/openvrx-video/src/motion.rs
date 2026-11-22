// SPDX-License-Identifier: GPL-3.0-or-later
//! Block motion estimation shared by the motion-compensated deinterlacer
//! and the motion-compensated temporal denoiser.
//!
//! For each block of the current plane the estimator finds the
//! displacement into the reference plane with the smallest mean absolute
//! difference plus a small vector-length penalty. Search: candidate
//! vectors (zero, the co-located vector of the previous estimate, the
//! three vectors of the block row above), a subsampled coarse grid over
//! the whole range, a diamond refinement at full resolution, and a final
//! half-sample refinement with bilinear interpolation.
//!
//! Rows are estimated in order (the row above feeds candidates) while the
//! blocks of one row are independent, so the result does not depend on
//! how many threads ran or how the rows were batched.

use rayon::prelude::*;

/// Read-only view of one plane.
#[derive(Copy, Clone)]
pub struct Plane<'a> {
    pub data: &'a [f32],
    pub width: usize,
    pub height: usize,
}

impl<'a> Plane<'a> {
    pub fn new(data: &'a [f32], width: usize, height: usize) -> Plane<'a> {
        debug_assert!(data.len() >= width * height);
        Plane { data, width, height }
    }

    #[inline]
    pub fn at(&self, x: isize, y: isize) -> f32 {
        let x = x.clamp(0, self.width as isize - 1) as usize;
        let y = y.clamp(0, self.height as isize - 1) as usize;
        self.data[y * self.width + x]
    }

    /// Bilinear sample at a fractional position (clamped edges).
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        let xf = x.floor();
        let yf = y.floor();
        let tx = x - xf;
        let ty = y - yf;
        let (xi, yi) = (xf as isize, yf as isize);
        let a = self.at(xi, yi);
        let b = self.at(xi + 1, yi);
        let c = self.at(xi, yi + 1);
        let d = self.at(xi + 1, yi + 1);
        let top = a + (b - a) * tx;
        let bot = c + (d - c) * tx;
        top + (bot - top) * ty
    }
}

/// One block's vector (reference position = block position + vector) and
/// its matching cost (mean absolute difference).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct MotionVector {
    pub dx: f32,
    pub dy: f32,
    pub cost: f32,
}

/// Search settings.
#[derive(Copy, Clone, Debug)]
pub struct SearchParams {
    pub block_w: usize,
    pub block_h: usize,
    pub range_x: i32,
    pub range_y: i32,
    /// Penalty per sample of vector length (in mean-absolute-difference
    /// units), favours smooth, short vectors in flat areas.
    pub lambda: f32,
    pub half_pel: bool,
    /// Mean absolute difference at or below which the best candidate
    /// (zero, previous or neighbouring vectors) is taken without the grid
    /// search: a match that good is as close as the noise allows. 0 always
    /// searches.
    pub good_enough: f32,
}

/// A field of block vectors.
#[derive(Clone, Debug, Default)]
pub struct MotionField {
    pub cols: usize,
    pub rows: usize,
    pub block_w: usize,
    pub block_h: usize,
    pub mv: Vec<MotionVector>,
}

impl MotionField {
    pub fn new(width: usize, height: usize, sp: &SearchParams) -> MotionField {
        let cols = width.div_ceil(sp.block_w);
        let rows = height.div_ceil(sp.block_h);
        MotionField {
            cols,
            rows,
            block_w: sp.block_w,
            block_h: sp.block_h,
            mv: vec![MotionVector::default(); cols * rows],
        }
    }

    #[inline]
    pub fn get(&self, col: usize, row: usize) -> MotionVector {
        self.mv[row.min(self.rows - 1) * self.cols + col.min(self.cols - 1)]
    }

    /// Vector at a pixel, bilinearly interpolated between block centres
    /// (smooth compensation without block edges).
    pub fn at_pixel(&self, x: usize, y: usize) -> (f32, f32) {
        let fx = (x as f32 + 0.5) / self.block_w as f32 - 0.5;
        let fy = (y as f32 + 0.5) / self.block_h as f32 - 0.5;
        let cx = fx.floor().max(0.0);
        let cy = fy.floor().max(0.0);
        let tx = (fx - cx).clamp(0.0, 1.0);
        let ty = (fy - cy).clamp(0.0, 1.0);
        let (c0, r0) = (cx as usize, cy as usize);
        let a = self.get(c0, r0);
        let b = self.get(c0 + 1, r0);
        let c = self.get(c0, r0 + 1);
        let d = self.get(c0 + 1, r0 + 1);
        let lerp = |p: f32, q: f32, t: f32| p + (q - p) * t;
        (
            lerp(lerp(a.dx, b.dx, tx), lerp(c.dx, d.dx, tx), ty),
            lerp(lerp(a.dy, b.dy, tx), lerp(c.dy, d.dy, tx), ty),
        )
    }
}

/// Sum of |a - b| over two equal-length rows. Eight partial sums, so the
/// compiler can keep them in one vector register (a single running f32 sum
/// cannot be vectorised: float addition is not reassociated).
#[inline(always)]
fn sad_row(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    let mut acc = [0.0f32; 8];
    let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
    let (ra, rb) = (ca.remainder(), cb.remainder());
    for (x, y) in ca.zip(cb) {
        for i in 0..8 {
            acc[i] += (x[i] - y[i]).abs();
        }
    }
    let mut s = ((acc[0] + acc[4]) + (acc[1] + acc[5])) + ((acc[2] + acc[6]) + (acc[3] + acc[7]));
    for (x, y) in ra.iter().zip(rb) {
        s += (x - y).abs();
    }
    s
}

/// Mean absolute difference of a block at an integer displacement. `step`
/// 2 takes every other row (the coarse search; whole rows keep the inner
/// loop contiguous).
#[allow(clippy::too_many_arguments)]
fn sad_int(cur: &Plane, refp: &Plane, bx: usize, by: usize, bw: usize, bh: usize, dx: i32, dy: i32, step: usize) -> f32 {
    let x1 = (bx + bw).min(cur.width);
    let y1 = (by + bh).min(cur.height);
    let rx0 = bx as isize + dx as isize;
    let ry0 = by as isize + dy as isize;
    let inside = rx0 >= 0
        && ry0 >= 0
        && (x1 as isize + dx as isize) <= refp.width as isize
        && (y1 as isize + dy as isize) <= refp.height as isize;
    let mut acc = 0.0f32;
    let mut n = 0usize;
    let mut y = by;
    while y < y1 {
        let crow = &cur.data[y * cur.width + bx..y * cur.width + x1];
        if inside {
            let ry = (y as isize + dy as isize) as usize;
            let rs = ry * refp.width + rx0 as usize;
            acc += sad_row(crow, &refp.data[rs..rs + crow.len()]);
        } else {
            for (i, c) in crow.iter().enumerate() {
                acc += (c - refp.at((bx + i) as isize + dx as isize, y as isize + dy as isize)).abs();
            }
        }
        n += crow.len();
        y += step;
    }
    acc / n.max(1) as f32
}

/// Mean absolute difference at a fractional displacement, bilinear
/// reference samples. The weights are the same for every sample of the
/// block, so inside the plane each row is four shifted reference rows.
#[allow(clippy::too_many_arguments)]
fn sad_frac(cur: &Plane, refp: &Plane, bx: usize, by: usize, bw: usize, bh: usize, dx: f32, dy: f32) -> f32 {
    let x1 = (bx + bw).min(cur.width);
    let y1 = (by + bh).min(cur.height);
    let (fx, fy) = (dx.floor(), dy.floor());
    let (tx, ty) = (dx - fx, dy - fy);
    let (ix, iy) = (fx as isize, fy as isize);
    let inside = bx as isize + ix >= 0
        && by as isize + iy >= 0
        && x1 as isize + ix < refp.width as isize
        && y1 as isize + iy < refp.height as isize;
    let mut acc = 0.0f32;
    let mut n = 0usize;
    if inside {
        let len = x1 - bx;
        let mut row = [0.0f32; 64];
        let mut scratch;
        let tmp: &mut [f32] = if len <= row.len() {
            &mut row[..len]
        } else {
            scratch = vec![0.0f32; len];
            &mut scratch[..]
        };
        for y in by..y1 {
            let r0 = ((y as isize + iy) as usize) * refp.width + (bx as isize + ix) as usize;
            let r1 = r0 + refp.width;
            let (a, b) = (&refp.data[r0..r0 + len], &refp.data[r0 + 1..r0 + 1 + len]);
            let (c, d) = (&refp.data[r1..r1 + len], &refp.data[r1 + 1..r1 + 1 + len]);
            // Same arithmetic as `Plane::sample`, so both paths agree exactly.
            for i in 0..len {
                let top = a[i] + (b[i] - a[i]) * tx;
                let bot = c[i] + (d[i] - c[i]) * tx;
                tmp[i] = top + (bot - top) * ty;
            }
            acc += sad_row(&cur.data[y * cur.width + bx..y * cur.width + x1], tmp);
            n += len;
        }
    } else {
        for y in by..y1 {
            for x in bx..x1 {
                acc += (cur.data[y * cur.width + x] - refp.sample(x as f32 + dx, y as f32 + dy)).abs();
                n += 1;
            }
        }
    }
    acc / n.max(1) as f32
}

/// Estimates one block.
fn estimate_block(cur: &Plane, refp: &Plane, sp: &SearchParams, col: usize, row: usize, cands: &[(i32, i32)]) -> MotionVector {
    let bx = col * sp.block_w;
    let by = row * sp.block_h;
    let (bw, bh) = (sp.block_w, sp.block_h);
    let pen = |dx: f32, dy: f32| sp.lambda * (dx.abs() + dy.abs());
    let cost_int = |dx: i32, dy: i32, step: usize| sad_int(cur, refp, bx, by, bw, bh, dx, dy, step) + pen(dx as f32, dy as f32);
    let clampv = |dx: i32, dy: i32| (dx.clamp(-sp.range_x, sp.range_x), dy.clamp(-sp.range_y, sp.range_y));
    // Full-resolution costs are remembered per vector: the candidates, the
    // two refinements and their repeated passes keep landing on vectors
    // already measured. Every vector here is clamped into the window.
    let wx = (2 * sp.range_x + 1) as usize;
    let memo = std::cell::RefCell::new(vec![f32::NAN; wx * (2 * sp.range_y + 1) as usize]);
    let full = |dx: i32, dy: i32| -> f32 {
        let i = (dy + sp.range_y) as usize * wx + (dx + sp.range_x) as usize;
        let mut m = memo.borrow_mut();
        if m[i].is_nan() {
            m[i] = cost_int(dx, dy, 1);
        }
        m[i]
    };
    let mut best = (0i32, 0i32);
    let mut best_cost = full(0, 0);
    for &(cx, cy) in cands {
        let (cx, cy) = clampv(cx, cy);
        if (cx, cy) == (0, 0) {
            continue;
        }
        let c = full(cx, cy);
        if c < best_cost {
            best_cost = c;
            best = (cx, cy);
        }
    }
    // A candidate that already matches to the noise level only gets a
    // one-sample refinement; otherwise the coarse grid searches the range.
    let settled = sp.good_enough > 0.0 && best_cost - pen(best.0 as f32, best.1 as f32) <= sp.good_enough;
    // Coarse grid, subsampled.
    let gx = (sp.range_x / 4).max(1);
    let gy = (sp.range_y / 4).max(1);
    let mut coarse_best = best;
    let mut coarse_cost = f32::INFINITY;
    let mut ty = -sp.range_y;
    while !settled && ty <= sp.range_y {
        let mut tx = -sp.range_x;
        while tx <= sp.range_x {
            let c = cost_int(tx, ty, 2);
            if c < coarse_cost {
                coarse_cost = c;
                coarse_best = (tx, ty);
            }
            tx += gx;
        }
        ty += gy;
    }
    let (starts, steps): (&[(i32, i32)], &[i32]) = if settled {
        (&[best], &[1])
    } else {
        (&[best, coarse_best], &[gx.max(gy).max(2) / 2, 1, 1])
    };
    // Refine around both the candidate winner and the coarse winner.
    for &start in starts {
        let mut pos = start;
        let mut cost = full(pos.0, pos.1);
        for &step in steps {
            let step = step.max(1);
            loop {
                let mut improved = false;
                for (ox, oy) in [(step, 0), (-step, 0), (0, step), (0, -step), (step, step), (-step, -step), (step, -step), (-step, step)] {
                    let (nx, ny) = clampv(pos.0 + ox, pos.1 + oy);
                    if (nx, ny) == pos {
                        continue;
                    }
                    let c = full(nx, ny);
                    if c < cost {
                        cost = c;
                        pos = (nx, ny);
                        improved = true;
                    }
                }
                if !improved {
                    break;
                }
            }
        }
        if cost < best_cost {
            best_cost = cost;
            best = pos;
        }
    }
    let mut mv = MotionVector {
        dx: best.0 as f32,
        dy: best.1 as f32,
        cost: best_cost,
    };
    if sp.half_pel {
        let (bx0, by0) = (mv.dx, mv.dy);
        for (ox, oy) in [(0.5f32, 0.0f32), (-0.5, 0.0), (0.0, 0.5), (0.0, -0.5), (0.5, 0.5), (-0.5, -0.5), (0.5, -0.5), (-0.5, 0.5)] {
            let (nx, ny) = (bx0 + ox, by0 + oy);
            let c = sad_frac(cur, refp, bx, by, bw, bh, nx, ny) + pen(nx, ny);
            if c < mv.cost {
                mv = MotionVector { dx: nx, dy: ny, cost: c };
            }
        }
    }
    mv.cost -= pen(mv.dx, mv.dy);
    mv
}

/// Estimates one row of blocks given the row above (already estimated in
/// `field`) and an optional previous motion field.
pub fn estimate_row(cur: &Plane, refp: &Plane, sp: &SearchParams, prev: Option<&MotionField>, field: &mut MotionField, row: usize, parallel: bool) {
    let cols = field.cols;
    let above: Vec<MotionVector> = if row > 0 {
        field.mv[(row - 1) * cols..row * cols].to_vec()
    } else {
        Vec::new()
    };
    let job = |col: usize| -> MotionVector {
        let mut cands: Vec<(i32, i32)> = Vec::with_capacity(6);
        if let Some(p) = prev {
            if p.cols == cols && p.rows == field.rows {
                let v = p.get(col, row);
                cands.push((v.dx.round() as i32, v.dy.round() as i32));
            }
        }
        if !above.is_empty() {
            for c in [col.saturating_sub(1), col, (col + 1).min(cols - 1)] {
                let v = above[c];
                cands.push((v.dx.round() as i32, v.dy.round() as i32));
            }
        }
        estimate_block(cur, refp, sp, col, row, &cands)
    };
    let out = &mut field.mv[row * cols..(row + 1) * cols];
    if parallel {
        let v: Vec<MotionVector> = (0..cols).into_par_iter().map(job).collect();
        out.copy_from_slice(&v);
    } else {
        for (col, o) in out.iter_mut().enumerate() {
            *o = job(col);
        }
    }
}

/// Estimates a whole motion field.
pub fn estimate(cur: &Plane, refp: &Plane, sp: &SearchParams, prev: Option<&MotionField>, parallel: bool) -> MotionField {
    let mut f = MotionField::new(cur.width, cur.height, sp);
    for row in 0..f.rows {
        estimate_row(cur, refp, sp, prev, &mut f, row, parallel);
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_shift() {
        let (w, h) = (160usize, 96usize);
        let tex = |x: f32, y: f32| ((x * 0.31).sin() * (y * 0.23).cos() + (x * 0.07 + y * 0.11).sin()) * 0.5;
        let refd: Vec<f32> = (0..w * h).map(|i| tex((i % w) as f32, (i / w) as f32)).collect();
        // Current = reference shifted: cur(x, y) = ref(x + 5.5, y - 3).
        let curd: Vec<f32> = (0..w * h).map(|i| tex((i % w) as f32 + 5.5, (i / w) as f32 - 3.0)).collect();
        let sp = SearchParams {
            block_w: 16,
            block_h: 16,
            range_x: 12,
            range_y: 8,
            lambda: 0.0005,
            half_pel: true,
            good_enough: 0.0,
        };
        let f = estimate(&Plane::new(&curd, w, h), &Plane::new(&refd, w, h), &sp, None, true);
        for r in 1..f.rows - 1 {
            for c in 1..f.cols - 1 {
                let v = f.get(c, r);
                assert!((v.dx - 5.5).abs() < 0.01 && (v.dy + 3.0).abs() < 0.01, "{c},{r}: {v:?}");
            }
        }
        // With an early exit: a zero vector far from the match still gets the
        // full search, and the previous field's vectors are kept as they are.
        let quick = SearchParams { good_enough: 0.02, ..sp };
        let g = estimate(&Plane::new(&curd, w, h), &Plane::new(&refd, w, h), &quick, None, true);
        let g2 = estimate(&Plane::new(&curd, w, h), &Plane::new(&refd, w, h), &quick, Some(&g), true);
        for field in [&g, &g2] {
            for r in 1..field.rows - 1 {
                for c in 1..field.cols - 1 {
                    let v = field.get(c, r);
                    assert!((v.dx - 5.5).abs() < 0.01 && (v.dy + 3.0).abs() < 0.01, "{c},{r}: {v:?}");
                }
            }
        }
    }

    #[test]
    fn fast_sums_match_the_plain_ones() {
        let (w, h) = (97usize, 41usize);
        let a: Vec<f32> = (0..w * h).map(|i| ((i * 37 % 101) as f32 * 0.013).sin()).collect();
        let b: Vec<f32> = (0..w * h).map(|i| ((i * 53 % 89) as f32 * 0.017).cos()).collect();
        let (pa, pb) = (Plane::new(&a, w, h), Plane::new(&b, w, h));
        let plain = |bx: usize, by: usize, dx: f32, dy: f32| {
            let mut s = 0.0f64;
            let mut n = 0;
            for y in by..(by + 16).min(h) {
                for x in bx..(bx + 16).min(w) {
                    s += (pa.data[y * w + x] - pb.sample(x as f32 + dx, y as f32 + dy)).abs() as f64;
                    n += 1;
                }
            }
            (s / n as f64) as f32
        };
        // Inside the plane, at the edges (clamped path) and at fractional
        // displacements.
        for (bx, by) in [(20, 10), (0, 0), (90, 30), (40, 25)] {
            for (dx, dy) in [(0.0f32, 0.0f32), (3.0, -2.0), (-5.0, 4.0), (2.5, -1.5), (-0.5, 0.5), (7.5, 3.0)] {
                let want = plain(bx, by, dx, dy);
                let got = if dx.fract() == 0.0 && dy.fract() == 0.0 {
                    sad_int(&pa, &pb, bx, by, 16, 16, dx as i32, dy as i32, 1)
                } else {
                    sad_frac(&pa, &pb, bx, by, 16, 16, dx, dy)
                };
                assert!((got - want).abs() < 1e-5, "({bx},{by}) ({dx},{dy}): {got} vs {want}");
            }
        }
    }
}
