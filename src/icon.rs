// Spoon logo, defined in code: a scalloped Material "cookie" with a tilted spoon.
// Shared with build.rs (via `include!`), so this file must stay dependency-free.
// Coordinates are in icon units: the icon spans -0.5..0.5 on both axes, y down.

pub const BRAND_BG: [u8; 3] = [0x4F, 0x46, 0xE5];
pub const BRAND_FG: [u8; 3] = [0xF5, 0xF3, 0xFF];

const COOKIE_RADIUS: f32 = 0.485;
const COOKIE_LOBES: f32 = 12.0;
const COOKIE_DEPTH: f32 = 0.032;
// Teardrop bowl: centre along the handle axis, half width, tip radius, neck radius.
const BOWL: (f32, f32, f32, f32) = (-0.135, 0.122, 0.150, 0.215);
const HANDLE: (f32, f32, f32, f32) = (0.0, 0.315, 0.027, 0.043); // from, to, half widths
// Soft shine inside the bowl: centre (across, along), radii, tint towards the background.
const SHINE: (f32, f32, f32, f32, f32) = (-0.036, -0.170, 0.036, 0.074, 0.16);

fn cookie_radius(angle: f32) -> f32 {
    COOKIE_RADIUS * (1.0 - COOKIE_DEPTH + COOKIE_DEPTH * (COOKIE_LOBES * angle).cos())
}

fn in_ellipse(u: f32, v: f32, centre: (f32, f32), ru: f32, rv: f32) -> bool {
    ((u - centre.0) / ru).powi(2) + ((v - centre.1) / rv).powi(2) <= 1.0
}

/// 0 = outside, 1 = background, 2 = spoon, 3 = shine on the bowl.
fn region(x: f32, y: f32) -> u8 {
    if x.hypot(y) > cookie_radius(y.atan2(x)) {
        return 0;
    }
    // Spoon frame: `v` runs from the bowl (top right) down the handle (bottom left).
    let s = std::f32::consts::FRAC_1_SQRT_2;
    let (u, v) = ((x + y) * s, (y - x) * s);
    if in_ellipse(u, v, (SHINE.0, SHINE.1), SHINE.2, SHINE.3) {
        return 3;
    }
    let (from, to, w0, w1) = HANDLE;
    let half_width = w0 + (w1 - w0) * ((v - from) / (to - from));
    let on_handle = v >= from && v <= to && u.abs() <= half_width;
    let on_cap = u.hypot(v - to) <= w1;
    let bowl_rv = if v < BOWL.0 { BOWL.2 } else { BOWL.3 };
    if in_ellipse(u, v, (0.0, BOWL.0), BOWL.1, bowl_rv) || on_handle || on_cap { 2 } else { 1 }
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [f32; 3] {
    [0, 1, 2].map(|i| a[i] as f32 + (b[i] as f32 - a[i] as f32) * t)
}

/// RGBA pixels (straight alpha), 4x4 supersampled.
pub fn render(size: usize, bg: [u8; 3], fg: [u8; 3]) -> Vec<u8> {
    const SS: usize = 4;
    let palette = [[0.0; 3], mix(bg, bg, 0.0), mix(fg, fg, 0.0), mix(fg, bg, SHINE.4)];
    let mut pixels = Vec::with_capacity(size * size * 4);
    for py in 0..size {
        for px in 0..size {
            let (mut sum, mut hits) = ([0.0f32; 3], 0.0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32 - 0.5;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32 - 0.5;
                    let r = region(x, y) as usize;
                    if r > 0 {
                        hits += 1.0;
                        sum = [0, 1, 2].map(|i| sum[i] + palette[r][i]);
                    }
                }
            }
            let color = sum.map(|c| if hits > 0.0 { (c / hits).round() as u8 } else { 0 });
            pixels.extend_from_slice(&color);
            pixels.push((hits / (SS * SS) as f32 * 255.0).round() as u8);
        }
    }
    pixels
}

/// Windows .ico with one 32-bit BMP image per size.
pub fn ico(sizes: &[usize]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&size| {
            let rgba = render(size, BRAND_BG, BRAND_FG);
            let mut bmp = Vec::new();
            let header: [u32; 10] = [40, size as u32, 2 * size as u32, 1 | 32 << 16, 0, 0, 0, 0, 0, 0];
            header.iter().for_each(|v| bmp.extend_from_slice(&v.to_le_bytes()));
            for row in (0..size).rev() {
                for px in rgba[row * size * 4..(row + 1) * size * 4].chunks(4) {
                    bmp.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
            }
            bmp.resize(bmp.len() + size.div_ceil(32) * 4 * size, 0); // AND mask
            bmp
        })
        .collect();

    let mut out = vec![0, 0, 1, 0];
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (size, image) in sizes.iter().zip(&images) {
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += image.len();
    }
    images.iter().for_each(|image| out.extend_from_slice(image));
    out
}

/// The same logo as a 512px SVG.
pub fn svg() -> String {
    let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
    let shine = mix(BRAND_FG, BRAND_BG, SHINE.4).map(|c| c.round() as u8);
    let px = |v: f32| v * 512.0;
    let cookie: Vec<String> = (0..240)
        .map(|i| {
            let a = i as f32 / 240.0 * std::f32::consts::TAU;
            let r = cookie_radius(a);
            format!("{:.1},{:.1}", 256.0 + px(r * a.cos()), 256.0 + px(r * a.sin()))
        })
        .collect();
    let (from, to, w0, w1) = HANDLE;
    let (bowl, bu, tip, neck) = (px(BOWL.0), px(BOWL.1), px(BOWL.2), px(BOWL.3));
    let lines = [
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512">"#.to_string(),
        "  <title>Spoon</title>".to_string(),
        format!(r#"  <polygon fill="{}" points="{}"/>"#, hex(BRAND_BG), cookie.join(" ")),
        format!(r#"  <g transform="translate(256 256) rotate(45)" fill="{}">"#, hex(BRAND_FG)),
        format!(r#"    <polygon points="{:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1}"/>"#, px(-w0), px(from), px(w0), px(from), px(w1), px(to), px(-w1), px(to)),
        format!(r#"    <circle cy="{:.1}" r="{:.1}"/>"#, px(to), px(w1)),
        format!(r#"    <path d="M{:.1},{bowl:.1} A{bu:.1},{tip:.1} 0 0 1 {bu:.1},{bowl:.1} A{bu:.1},{neck:.1} 0 0 1 {:.1},{bowl:.1} Z"/>"#, -bu, -bu),
        format!(r#"    <ellipse cx="{:.1}" cy="{:.1}" rx="{:.1}" ry="{:.1}" fill="{}"/>"#, px(SHINE.0), px(SHINE.1), px(SHINE.2), px(SHINE.3), hex(shine)),
        "  </g>".to_string(),
        "</svg>".to_string(),
    ];
    lines.join("\n") + "\n"
}
