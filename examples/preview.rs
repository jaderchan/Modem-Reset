//! 图标预览：把各档尺寸的原始 BGRA 写到 target/icon-preview/ 下。
//!
//! 用途是调整 `src/raster.rs` 的绘制代码时，可以直接看效果：
//!
//! ```powershell
//! cargo run --release --example preview
//! powershell -ExecutionPolicy Bypass -File examples\preview.ps1
//! start target\icon-preview\sheet.png
//! ```
//!
//! 第一个命令导出裸像素（每像素 4 字节 BGRA），第二个把它们拼成一张
//! 放大 8 倍的对比图 `sheet.png`，小尺寸有没有模糊，一眼就能看出。

mod raster {
    include!("../src/raster.rs");
}

fn main() {
    let dir = std::path::Path::new("target/icon-preview");
    std::fs::create_dir_all(dir).expect("创建 target/icon-preview 失败");

    for &size in raster::ICO_SIZES.iter().chain(&[20, 40]) {
        let px = raster::render(size);
        let path = dir.join(format!("{size}.bgra"));
        std::fs::write(&path, &px).expect("写入预览文件失败");
        println!("{} {}x{} ({} bytes)", path.display(), size, size, px.len());
    }
}
