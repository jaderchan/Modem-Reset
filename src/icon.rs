//! 运行时把画好的像素变成托盘用的 HICON。
//!
//! 图像本身由 `raster` 模块生成（与嵌进 exe 的 `app.ico` 同源），
//! 这里只负责写入 32bpp DIB，再转成图标句柄。

use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

use crate::raster;

/// 画一个 size×size 的电源图标。调用方需保证 size >= 16。
pub fn power_icon(size: u32) -> HICON {
    let pixels = raster::render(size);

    // SAFETY: 所有句柄都由本函数创建、在返回前释放；DIB 像素缓冲区在 CreateIconIndirect
    // 之前一直有效，且长度与 biWidth*biHeight*4 严格一致。
    unsafe {
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            biHeight: -(size as i32), // 负数表示自上而下，可以省去翻转
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };

        let hdc = CreateCompatibleDC(std::ptr::null_mut());
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(
            hdc,
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if color.is_null() || bits.is_null() {
            if !hdc.is_null() {
                DeleteDC(hdc);
            }
            return std::ptr::null_mut();
        }
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
        DeleteDC(hdc);

        // 全零的 AND 掩码表示每个像素都由颜色位图决定，透明交给 alpha 通道。
        // 必须自己传缓冲区：CreateBitmap 收到空指针时内容是不确定的。
        // 1bpp 每行按 32 位对齐。
        let stride = (size as usize).div_ceil(32) * 4;
        let zeros = vec![0u8; stride * size as usize];
        let mask = CreateBitmap(
            size as i32,
            size as i32,
            1,
            1,
            zeros.as_ptr() as *const core::ffi::c_void,
        );

        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info);

        DeleteObject(color as _);
        if !mask.is_null() {
            DeleteObject(mask as _);
        }
        icon
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Graphics::Gdi::{BITMAP, GetDIBits, GetObjectW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo};

    /// 回归测试：AND 掩码必须是全零，不能是 CreateBitmap 的未初始化内容。
    #[test]
    fn and_mask_is_all_zero() {
        let size = 32u32;
        let hicon = power_icon(size);
        assert!(!hicon.is_null(), "power_icon 返回了空句柄");

        // SAFETY: 句柄都由本测试创建或取出，用完即释放；缓冲区长度按位图尺寸算。
        unsafe {
            let mut info: ICONINFO = std::mem::zeroed();
            assert_eq!(GetIconInfo(hicon, &mut info), 1, "GetIconInfo 失败");

            let mut bm: BITMAP = std::mem::zeroed();
            let got = GetObjectW(
                info.hbmMask as _,
                std::mem::size_of::<BITMAP>() as i32,
                &mut bm as *mut BITMAP as *mut core::ffi::c_void,
            );
            assert_eq!(got, std::mem::size_of::<BITMAP>() as i32, "读 BITMAP 失败");
            assert_eq!(
                (bm.bmWidth, bm.bmHeight, bm.bmBitsPixel),
                (size as i32, size as i32, 1),
                "掩码尺寸或位深不对"
            );

            let stride = (size as usize).div_ceil(32) * 4;
            let mut bits = vec![0xFFu8; stride * size as usize];
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: size as i32, // 正数表示自下而上，读回来不关心行序
                biPlanes: 1,
                biBitCount: 1,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            };
            let hdc = CreateCompatibleDC(std::ptr::null_mut());
            let rows = GetDIBits(
                hdc,
                info.hbmMask,
                0,
                size,
                bits.as_mut_ptr() as *mut core::ffi::c_void,
                &mut bmi,
                DIB_RGB_COLORS,
            );
            DeleteDC(hdc);

            if !info.hbmMask.is_null() {
                DeleteObject(info.hbmMask as _);
            }
            if !info.hbmColor.is_null() {
                DeleteObject(info.hbmColor as _);
            }
            DestroyIcon(hicon);

            assert_eq!(rows, size as i32, "GetDIBits 没读满");
            assert!(bits.iter().all(|&b| b == 0), "AND 掩码应为全零");
        }
    }
}
