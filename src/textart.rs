//! Album art in braille: each cell's 2×4 dots follow the picture's
//! brightness (stretched to its full range, then Floyd–Steinberg dithered),
//! coloured by the pixels under the lit dots. On a dark background the
//! light parts of the cover are what show.

/// An image as RGBA pixels, row by row.
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub px: Vec<[u8; 4]>,
}

/// One cell of braille art.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub row: u32,
    pub col: u32,
    pub ch: char,
    pub rgb: u32,
}

/// Braille dot bits, by (column, row) within the cell.
const DOT: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// `img` fitted into `cols`×`rows` cells, centred, keeping its shape.
/// `cell_px` is a cell's (height, width) in pixels. Blank cells are left out.
pub fn braille(img: &Image, cols: u32, rows: u32, cell_px: (u32, u32)) -> Vec<Cell> {
    if img.w == 0 || img.h == 0 || cols == 0 || rows == 0 {
        return Vec::new();
    }
    // A dot's height over its width on screen (1 for 20×10 px cells).
    let dot_aspect = f64::from(cell_px.0) / 4.0 / (f64::from(cell_px.1) / 2.0);
    let (gw, gh) = (cols * 2, rows * 4);
    // Dots the image covers, in whole cells, and where they start.
    let scale = (f64::from(gw) / f64::from(img.w)).min(f64::from(gh) * dot_aspect / f64::from(img.h));
    let uw = ((f64::from(img.w) * scale).round() as u32).clamp(2, gw) & !1;
    let uh = ((f64::from(img.h) * scale / dot_aspect).round() as u32).clamp(4, gh) & !3;
    let (ox, oy) = (((gw - uw) / 2) & !1, ((gh - uh) / 2) & !3);

    // Each dot: the average of the pixels it covers.
    let mut rgb = Vec::with_capacity((uw * uh) as usize);
    for dy in 0..uh {
        let (y0, y1) = span(dy, uh, img.h);
        for dx in 0..uw {
            let (x0, x1) = span(dx, uw, img.w);
            let mut sum = [0u64; 3];
            for y in y0..y1 {
                for p in &img.px[(y * img.w + x0) as usize..(y * img.w + x1) as usize] {
                    // transparent counts as the (dark) background
                    let a = u64::from(p[3]);
                    for c in 0..3 {
                        sum[c] += u64::from(p[c]) * a / 255;
                    }
                }
            }
            let n = u64::from((y1 - y0) * (x1 - x0));
            rgb.push(sum.map(|s| (s / n) as u8));
        }
    }

    // Brightness, stretched so the cover's darkest and lightest parts
    // (ignoring the extreme 2%) span the whole range.
    let lum: Vec<f32> =
        rgb.iter().map(|&[r, g, b]| 0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)).collect();
    let mut sorted = lum.clone();
    sorted.sort_by(f32::total_cmp);
    let pick = |q: f32| sorted[((sorted.len() - 1) as f32 * q) as usize];
    // (a nearly flat image keeps its own brightness)
    let (lo, hi) = match (pick(0.02), pick(0.98)) {
        (lo, hi) if hi - lo >= 32.0 => (lo, hi),
        _ => (0.0, 255.0),
    };
    let range = hi - lo;
    let level: Vec<f32> = lum.iter().map(|l| ((l - lo) / range).clamp(0.0, 1.0)).collect();
    let mut level = sharpen(&level, uw as usize, uh as usize);

    // Floyd–Steinberg: a dot is lit when its level, plus the error carried
    // from its neighbours, passes one half.
    let (w, h) = (uw as usize, uh as usize);
    let mut lit = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let on = level[i] >= 0.5;
            lit[i] = on;
            let err = level[i] - if on { 1.0 } else { 0.0 };
            let mut spread = |dx: isize, dy: usize, k: f32| {
                let nx = x as isize + dx;
                if (0..w as isize).contains(&nx) && y + dy < h {
                    level[(y + dy) * w + nx as usize] += err * k;
                }
            };
            spread(1, 0, 7.0 / 16.0);
            spread(-1, 1, 3.0 / 16.0);
            spread(0, 1, 5.0 / 16.0);
            spread(1, 1, 1.0 / 16.0);
        }
    }

    let mut cells = Vec::new();
    for cy in 0..uh / 4 {
        for cx in 0..uw / 2 {
            let (mut bits, mut sum, mut n) = (0, [0u32; 3], 0);
            for (col, dots) in DOT.iter().enumerate() {
                for (row, bit) in dots.iter().enumerate() {
                    let i = ((cy * 4 + row as u32) * uw + cx * 2 + col as u32) as usize;
                    if lit[i] {
                        bits |= bit;
                        for c in 0..3 {
                            sum[c] += u32::from(rgb[i][c]);
                        }
                        n += 1;
                    }
                }
            }
            if n > 0 {
                let [r, g, b] = sum.map(|s| s / n);
                cells.push(Cell {
                    row: (oy / 4) + cy,
                    col: (ox / 2) + cx,
                    ch: char::from_u32(0x2800 + bits).unwrap_or(' '),
                    rgb: (r << 16) | (g << 8) | b,
                });
            }
        }
    }
    cells
}

/// Unsharp mask: each dot pushed away from the average of the dots
/// around it, so edges and features survive the dithering.
fn sharpen(level: &[f32], w: usize, h: usize) -> Vec<f32> {
    const R: usize = 2;
    const AMOUNT: f32 = 1.5;
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let (ys, xs) = (y.saturating_sub(R)..(y + R + 1).min(h), x.saturating_sub(R)..(x + R + 1).min(w));
            let n = (ys.len() * xs.len()) as f32;
            let blur = ys.flat_map(|yy| xs.clone().map(move |xx| level[yy * w + xx])).sum::<f32>() / n;
            let l = level[y * w + x];
            (l + AMOUNT * (l - blur)).clamp(0.0, 1.0)
        })
        .collect()
}

/// The source pixels `[start, end)` behind dot `i` of `n`, along a side of
/// `len` pixels; never empty.
fn span(i: u32, n: u32, len: u32) -> (u32, u32) {
    let start = (u64::from(i) * u64::from(len) / u64::from(n)) as u32;
    let end = (u64::from(i + 1) * u64::from(len) / u64::from(n)) as u32;
    (start.min(len - 1), end.max(start + 1).min(len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Image {
        Image { w, h, px: (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| f(x, y)).collect() }
    }

    #[test]
    fn a_square_cover_fills_square_cells() {
        // 20×10 px cells: 10 cols × 5 rows is square on screen
        let img = solid(100, 100, |x, _| if x < 50 { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
        let cells = braille(&img, 10, 5, (20, 10));
        assert!(cells.iter().all(|c| c.row < 5 && c.col < 10));
        // the light right half is lit, the dark left half isn't
        assert!(cells.iter().all(|c| c.col >= 5), "{cells:?}");
        assert!(cells.iter().any(|c| c.ch == '⣿' && c.rgb == 0xffffff));
    }

    #[test]
    fn a_wide_cover_is_centred_vertically() {
        let img = solid(200, 100, |_, _| [255, 0, 0, 255]);
        let cells = braille(&img, 10, 10, (20, 10));
        let rows: Vec<u32> = cells.iter().map(|c| c.row).collect();
        let (lo, hi) = (*rows.iter().min().unwrap(), *rows.iter().max().unwrap());
        assert!(lo > 0 && hi < 9 && lo + hi == 9, "rows {lo}..={hi}");
    }

    #[test]
    fn mid_grey_dithers_to_about_half_the_dots() {
        let img = solid(64, 64, |x, y| if (x + y) % 2 == 0 { [0, 0, 0, 255] } else { [255, 255, 255, 255] });
        // the 2% stretch is a no-op on pure black and white
        let cells = braille(&img, 16, 8, (20, 10));
        let dots: u32 = cells.iter().map(|c| (c.ch as u32 - 0x2800).count_ones()).sum();
        let total = 16 * 8 * 8;
        assert!((dots as f32 / total as f32 - 0.5).abs() < 0.1, "{dots} of {total}");
    }
}
