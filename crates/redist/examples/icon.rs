//! Makes `assets/redist.ico`, the exes' icon, from the two drawings:
//! `icon-16.svg` for the smallest sizes and `icon.svg` for the rest.
//!
//! ```text
//! cargo run -p redist --example icon                 # writes assets/redist.ico
//! cargo run -p redist --example icon -- <folder>     # also every size as PNG,
//!                                                    # from both drawings
//! ```
//!
//! `build.rs` embeds the icon file in both exes as resource 1. Sizes below
//! 256 are stored as 32-bit bitmaps, 256 as PNG, as Windows' own icons are.

use std::fs;
use std::path::{Path, PathBuf};

use resvg::{tiny_skia, usvg};

/// Every size Windows asks for: 16 to 32 for title bars and small icons at
/// 100 to 200 % scale, 32 to 64 for the taskbar and the desktop, 256 for
/// Explorer's large views.
const SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

/// Sizes up to this one use the drawing made for the 16 px grid. From 20 px
/// on, icon.svg itself stays legible, and the 16 px drawing scaled up would
/// lose its sharp edges.
const SMALL_UP_TO: u32 = 16;

fn main() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let large = load(&assets.join("icon.svg"));
    let small = load(&assets.join("icon-16.svg"));
    let preview = std::env::args_os().nth(1).map(PathBuf::from);
    if let Some(folder) = &preview {
        fs::create_dir_all(folder).expect("the preview folder can be made");
    }

    let mut images = Vec::new();
    for size in SIZES {
        let pixels = render(if size <= SMALL_UP_TO { &small } else { &large }, size);
        images.push((size, pixels));
        if let Some(folder) = &preview {
            for (name, tree) in [("icon", &large), ("icon-16", &small)] {
                let file = folder.join(format!("{size}-{name}.png"));
                fs::write(&file, png(size, &render(tree, size))).expect("a preview PNG is written");
            }
        }
    }

    let ico = assets.join("redist.ico");
    fs::write(&ico, icon_file(&images)).expect("assets/redist.ico is written");
    println!("{} written: {SIZES:?} px", ico.display());
}

fn load(path: &Path) -> usvg::Tree {
    let data = fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    usvg::Tree::from_data(&data, &usvg::Options::default())
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The drawing at `size` × `size`, as straight (not premultiplied) RGBA.
fn render(tree: &usvg::Tree, size: u32) -> Vec<u8> {
    let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("a non-zero size");
    let scale = size as f32 / tree.size().width();
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect()
}

fn png(size: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("a PNG header");
    writer.write_image_data(rgba).expect("PNG data");
    writer.finish().expect("a finished PNG");
    out
}

/// A 32-bit bitmap as an icon stores it: a BITMAPINFOHEADER with twice the
/// height (the colour image, then the mask), BGRA rows bottom-up, then a
/// 1-bit mask that marks the fully transparent pixels.
fn bitmap(size: u32, rgba: &[u8]) -> Vec<u8> {
    let side = size as usize;
    let mask_row = side.div_ceil(32) * 4;
    let mut out = Vec::new();
    for value in [40, size, size * 2] {
        out.extend(value.to_le_bytes()); // biSize, biWidth, biHeight
    }
    out.extend(1u16.to_le_bytes()); // biPlanes
    out.extend(32u16.to_le_bytes()); // biBitCount
    out.extend(0u32.to_le_bytes()); // biCompression: BI_RGB
    out.extend(((side * side * 4 + mask_row * side) as u32).to_le_bytes()); // biSizeImage
    out.extend([0u8; 16]); // resolution and palette: unused
    for row in (0..side).rev() {
        for pixel in rgba[row * side * 4..(row + 1) * side * 4]
            .as_chunks::<4>()
            .0
        {
            out.extend([pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
    }
    for row in (0..side).rev() {
        let mut bits = vec![0u8; mask_row];
        for column in 0..side {
            if rgba[(row * side + column) * 4 + 3] == 0 {
                bits[column / 8] |= 0x80 >> (column % 8);
            }
        }
        out.extend(bits);
    }
    out
}

/// An .ico file: ICONDIR, one ICONDIRENTRY per image, then the images.
fn icon_file(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let data: Vec<Vec<u8>> = images
        .iter()
        .map(|(size, rgba)| {
            if *size >= 256 {
                png(*size, rgba)
            } else {
                bitmap(*size, rgba)
            }
        })
        .collect();
    let mut out = Vec::new();
    out.extend(0u16.to_le_bytes()); // reserved
    out.extend(1u16.to_le_bytes()); // type: icon
    out.extend((images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    for ((size, _), bytes) in images.iter().zip(&data) {
        let side = if *size >= 256 { 0 } else { *size as u8 }; // 0 means 256
        out.extend([side, side, 0, 0]); // width, height, colours, reserved
        out.extend(1u16.to_le_bytes()); // planes
        out.extend(32u16.to_le_bytes()); // bits per pixel
        out.extend((bytes.len() as u32).to_le_bytes());
        out.extend((offset as u32).to_le_bytes());
        offset += bytes.len();
    }
    for bytes in data {
        out.extend(bytes);
    }
    out
}
