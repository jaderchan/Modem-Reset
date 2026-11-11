// 纯 Rust 的图标光栅化 + `.ico` 帧封装。
//
// 这里刻意**不碰任何 Win32 API**：同一份代码既被 `icon.rs` 用来在运行时
// 生成托盘用的 HICON，也被 `build.rs` 用来生成嵌进 exe 的 `app.ico`，
// 保证两处画出来的图标完全一致。
//
// 注意：本文件会被 `include!` 进 build.rs 的模块里，所以顶部只能用 `//`
// 注释，不能用 `//!`（inner doc 在 include! 展开处是不合法的）。
//
// ICO 目录和单档帧都在本文件组装；`build.rs` 只负责把大尺寸换成 PNG 帧
// （`png` 只能作为构建依赖，不能进主程序）。

use std::f32::consts::PI;

/// 嵌进 exe 的各档尺寸（Explorer 会按视图挑最合适的一档）。
#[allow(dead_code)] // 只有 build.rs 用（它 include 了本文件）
pub const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

const BG: [f32; 3] = [17.0, 24.0, 39.0];
const FG: [f32; 3] = [248.0, 250.0, 252.0];

/// 画一个 size×size 的“电源符号”，返回 **BGRA、自上而下** 的像素。
pub fn render(size: u32) -> Vec<u8> {
    let n = size as usize;
    let mut out = vec![0u8; n * n * 4];

    let fsize = size as f32;
    let c = fsize / 2.0;
    let disc = c - 0.5;
    let ring_r = c * 0.60;
    // 16 px 下按比例算出来的线宽不足 1 像素，会粘连成一片，所以给个绝对下限：
    // 圆环整宽至少 2 px、竖线至少 2 px，这样缩到 16 px 还能看出“电源”的形状。
    let ring_w = (c * 0.115).max(1.0);
    let bar_half = (c * 0.08).max(0.85);
    // 缺口半角（弧度，0.62 约 35.5°）：太小的话，小尺寸下圆环两端会被抗锯齿抹掉
    let gap_half = 0.62;
    // 竖线从圆环外沿稍上方一直伸到接近圆心，和圆环组成完整的电源符号
    let bar_top = -(ring_r + ring_w) - 0.5;
    let bar_bottom = -c * 0.12;
    let supersample = 4;
    let total = (supersample * supersample) as f32;

    for y in 0..n {
        for x in 0..n {
            let (mut acc, mut hits) = ([0.0f32; 3], 0.0f32);

            for sy in 0..supersample {
                for sx in 0..supersample {
                    let px = x as f32 + (sx as f32 + 0.5) / supersample as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / supersample as f32;
                    let dx = px - c;
                    let dy = py - c;
                    let r = (dx * dx + dy * dy).sqrt();
                    if r > disc {
                        continue;
                    }

                    let mut color = BG;
                    // 圆环，顶部留缺口
                    let angle = dy.atan2(dx);
                    let in_gap = (angle + PI / 2.0).abs() < gap_half;
                    if (r - ring_r).abs() <= ring_w && !in_gap {
                        color = FG;
                    }
                    // 竖线：从缺口上沿伸到圆心附近
                    if dx.abs() <= bar_half && dy > bar_top && dy < bar_bottom {
                        color = FG;
                    }

                    for i in 0..3 {
                        acc[i] += color[i];
                    }
                    hits += 1.0;
                }
            }

            let alpha = hits / total;
            if alpha > 0.0 {
                let idx = (y * n + x) * 4;
                out[idx] = (acc[2] / hits) as u8; // B
                out[idx + 1] = (acc[1] / hits) as u8; // G
                out[idx + 2] = (acc[0] / hits) as u8; // R
                out[idx + 3] = (alpha * 255.0) as u8;
            }
        }
    }

    out
}

/// 把各档图像打包成标准 `.ico`。
///
/// `frames` 由调用方准备好，可以是 32bpp DIB（`dib`）也可以是 PNG 字节流。
/// ICO 两种都允许，解码方按帧头区分。`build.rs` 用“小尺寸 DIB + 大尺寸 PNG”。
#[allow(dead_code)] // 只有 build.rs 用
pub fn encode_ico(frames: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type = icon
    out.extend_from_slice(&(frames.len() as u16).to_le_bytes());

    let mut offset = 6 + 16 * frames.len() as u32;
    for (size, data) in frames {
        let dim = if *size >= 256 { 0u8 } else { *size as u8 }; // 256 记作 0
        out.push(dim); // width
        out.push(dim); // height
        out.push(0); // 调色板颜色数（32bpp 为 0）
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bit count
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }

    for (_, data) in frames {
        out.extend_from_slice(data);
    }
    out
}

/// 单档图像：BITMAPINFOHEADER + 自下而上的 BGRA + 全零 AND 掩码（透明交给 alpha）。
#[allow(dead_code)] // 只有 build.rs 用
pub fn dib(size: u32) -> Vec<u8> {
    let n = size as usize;
    let px = render(size);
    let row = n * 4;
    let mask_stride = n.div_ceil(32) * 4;

    let mut out = Vec::with_capacity(40 + n * row + mask_stride * n);

    // BITMAPINFOHEADER
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(size as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&((size as i32) * 2).to_le_bytes()); // biHeight = XOR + AND
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    out.extend_from_slice(&((n * row) as u32).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    // DIB 是自下而上存储，render() 给的是自上而下，这里逐行从下往上拷贝
    for y in (0..n).rev() {
        out.extend_from_slice(&px[y * row..y * row + row]);
    }

    // AND 掩码全零表示每个像素都由颜色位图决定，实际透明看 alpha
    out.resize(out.len() + mask_stride * n, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(px: &[u8], size: u32, x: u32, y: u32) -> u8 {
        px[((y * size + x) * 4 + 3) as usize]
    }

    #[test]
    fn renders_opaque_disc_with_transparent_corners() {
        for size in [16u32, 24, 32] {
            let px = render(size);
            assert_eq!(px.len(), (size * size * 4) as usize);
            assert!(alpha_at(&px, size, size / 2, size / 2) > 200, "size={size}");
            assert_eq!(alpha_at(&px, size, 0, 0), 0, "size={size}");
            assert_eq!(alpha_at(&px, size, size - 1, size - 1), 0, "size={size}");
        }
    }

    #[test]
    fn ring_uses_light_color() {
        let size = 32u32;
        let px = render(size);
        let x = size / 2;
        let found = (0..size).rev().any(|y| {
            let i = ((y * size + x) * 4) as usize;
            px[i + 3] > 128 && px[i] > 200 && px[i + 1] > 200 && px[i + 2] > 200
        });
        assert!(found, "中轴线上应能看到亮色的圆环");
    }

    #[test]
    fn ico_directory_matches_payload() {
        let ico = encode_ico(&[(16, dib(16)), (32, dib(32))]);
        assert_eq!(&ico[0..6], &[0, 0, 1, 0, 2, 0]); // 6 字节头，2 档
        let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
        assert_eq!(count, 2);

        // 每档的偏移和长度必须正好覆盖后面的数据
        let mut expected_offset = 6 + 16 * count;
        for i in 0..count {
            let base = 6 + 16 * i;
            let len = u32::from_le_bytes(ico[base + 8..base + 12].try_into().unwrap()) as usize;
            let off = u32::from_le_bytes(ico[base + 12..base + 16].try_into().unwrap()) as usize;
            assert_eq!(off, expected_offset);
            // 图像数据以 BITMAPINFOHEADER 的 biSize=40 开头
            assert_eq!(
                u32::from_le_bytes(ico[off..off + 4].try_into().unwrap()),
                40
            );
            expected_offset += len;
        }
        assert_eq!(expected_offset, ico.len());
    }
}
