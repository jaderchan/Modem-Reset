//! 构建脚本：把自绘图标打包成 exe 资源，顺带写版本信息。
//!
//! 光栅化代码直接 `include!` 自 `src/raster.rs`，和托盘图标同源，
//! 所以资源管理器里看到的图标和托盘里的图标是同一张图。
//!
//! ICO 目录和帧数据由 `raster::encode_ico` 组装；这里只负责把大尺寸换成
//! PNG 帧。`raster.rs` 同时是主程序的模块，不能依赖 `png`，所以 PNG 编码
//! 只能留在构建脚本里。

mod raster {
    include!("src/raster.rs");
}

use std::path::PathBuf;

/// 从这一档开始用 PNG 帧。16、24、32 太小，PNG 的头部开销反而更亏。
const PNG_FROM: u32 = 48;

fn main() {
    println!("cargo:rerun-if-changed=src/raster.rs");
    println!("cargo:rerun-if-changed=build.rs");

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR 未设置"));
    let ico = out.join("app.ico");
    std::fs::write(&ico, build_ico(&raster::ICO_SIZES)).expect("写入 app.ico 失败");

    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let (major, minor, patch) = split_version(&version);

    // rc.exe 解析相对路径时以它的当前工作目录为基准，这里一律用绝对路径和正斜杠
    let ico_path = ico.to_string_lossy().replace('\\', "/");
    let rc = out.join("app.rc");
    let script = format!(
        r#"#pragma code_page(65001)

1 ICON "{ico_path}"

1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEOS 0x40004L
FILETYPE 0x1L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "080404b0"
    BEGIN
      VALUE "CompanyName", "Modem-Reset"
      VALUE "FileDescription", "中兴 F612 光猫 / 小米路由器重启工具"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "ModemReset"
      VALUE "OriginalFilename", "ModemReset.exe"
      VALUE "ProductName", "Modem-Reset"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x804, 1200
  END
END
"#
    );
    std::fs::write(&rc, script).expect("写入 app.rc 失败");

    let _ = embed_resource::compile(&rc, embed_resource::NONE);
}

/// 按 `.ico` 格式打包各档：小尺寸存 32bpp DIB，大尺寸存 PNG。
///
/// 256 px 的未压缩 DIB 是 270 KB，压成 PNG 只剩 6 KB，这一项就占了整个资源区的
/// 三分之二。ICO 从 Vista 起允许 PNG 帧（Win7 及以上所有尺寸都支持），我们是 Win10+ 基线。
fn build_ico(sizes: &[u32]) -> Vec<u8> {
    let frames: Vec<(u32, Vec<u8>)> = sizes
        .iter()
        .map(|&s| {
            let data = if s >= PNG_FROM {
                png_frame(s)
            } else {
                raster::dib(s)
            };
            (s, data)
        })
        .collect();
    raster::encode_ico(&frames)
}

/// `raster::render` 给的是自上而下的 BGRA，PNG 要 RGBA，这里顺便交换 R 和 B。
fn png_frame(size: u32) -> Vec<u8> {
    let mut rgba = raster::render(size);
    let (pixels, _) = rgba.as_chunks_mut::<4>();
    for px in pixels {
        px.swap(0, 2);
    }

    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, size, size);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        // 图标只编一次，压缩率优先；对构建时间几乎没有影响
        enc.set_compression(png::Compression::High);
        let mut writer = enc.write_header().expect("写 PNG 头失败");
        writer.write_image_data(&rgba).expect("写 PNG 数据失败");
    }
    // 帧头是 PNG 签名，ICO 解码方靠它区分 DIB 和 PNG
    assert_eq!(&out[..8], b"\x89PNG\r\n\x1a\n", "png 帧格式异常");
    out
}

/// "2.0.0" -> (2, 0, 0)；VERSIONINFO 只认 u16，无法解析的部分按 0 处理。
fn split_version(version: &str) -> (u16, u16, u16) {
    let mut parts = version.split('.').map(|p| {
        p.chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse::<u16>()
            .unwrap_or(0)
    });
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}
