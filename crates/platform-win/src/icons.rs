//! App icons through `IShellItemImageFactory`, the same source Explorer and
//! the Start menu draw from, so MSIX tiles and Win32 shortcuts both work.

use std::{ffi::c_void, mem::size_of, thread};

use windows::{
    Win32::{
        Foundation::SIZE,
        Graphics::Gdi::{
            BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS,
            DeleteDC, DeleteObject, GetDIBits, GetObjectW, HBITMAP, HGDIOBJ,
        },
        System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx},
        UI::Shell::{
            IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK,
            SIIGBF_ICONONLY,
        },
    },
    core::{HSTRING, Result},
};

/// A decoded icon: `width * height` pixels, BGRA, straight (not premultiplied) alpha.
pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Extracts the icon of a shell item, `size` pixels square. `parsing_name`
/// is a file path, an absolute parsing name (`::{…}`), or
/// `shell:AppsFolder\\<launch id>` for an app. Blocking and COM-heavy: call it
/// from a thread that initialised COM as an STA.
pub fn shell_icon(parsing_name: &str, size: u32) -> Result<Icon> {
    let path = HSTRING::from(parsing_name);
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&path, None)?;
        let bitmap = factory.GetImage(
            SIZE {
                cx: size as i32,
                cy: size as i32,
            },
            SIIGBF_ICONONLY,
        )?;
        let icon = read_bitmap(bitmap);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        icon
    }
}

/// The thumbnail Explorer shows for a file (a picture, a video's frame, a
/// PDF's first page), fitting in `size` pixels, or its icon when it has none.
/// Blocking; initialises COM on the calling thread.
pub fn thumbnail(path: &str, size: u32) -> Result<Icon> {
    let path = HSTRING::from(path);
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&path, None)?;
        let bitmap = factory.GetImage(
            SIZE {
                cx: size as i32,
                cy: size as i32,
            },
            SIIGBF_BIGGERSIZEOK,
        )?;
        let icon = read_bitmap(bitmap);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        icon
    }
}

/// Extracts icons for `parsing_names` (see [`shell_icon`]) on a dedicated COM
/// thread and sends each one with its index in `parsing_names`. Items whose
/// icon can't be extracted are skipped.
pub fn spawn(parsing_names: Vec<String>, size: u32, icons: async_channel::Sender<(usize, Icon)>) {
    let spawned = thread::Builder::new().name("icons".into()).spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        }
        for (ix, parsing_name) in parsing_names.iter().enumerate() {
            match shell_icon(parsing_name, size) {
                Ok(icon) => {
                    if icons.send_blocking((ix, icon)).is_err() {
                        return;
                    }
                }
                Err(err) => eprintln!("[icons] no icon for {parsing_name:?}: {err}"),
            }
        }
    });
    if let Err(err) = spawned {
        eprintln!("[icons] could not start the icon thread: {err}");
    }
}

/// Copies a 32-bit bitmap out as top-down BGRA and undoes the premultiplied
/// alpha the shell hands out.
unsafe fn read_bitmap(bitmap: HBITMAP) -> Result<Icon> {
    unsafe {
        let mut info = BITMAP::default();
        if GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<BITMAP>() as i32,
            Some(&mut info as *mut _ as *mut c_void),
        ) == 0
        {
            return Err(windows::core::Error::from_win32());
        }
        let (width, height) = (info.bmWidth.max(0) as u32, info.bmHeight.max(0) as u32);

        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                // Negative height: rows top to bottom.
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bgra = vec![0u8; (width * height * 4) as usize];
        let dc = CreateCompatibleDC(None);
        let rows = GetDIBits(
            dc,
            bitmap,
            0,
            height,
            Some(bgra.as_mut_ptr().cast()),
            &mut header,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        if rows == 0 {
            return Err(windows::core::Error::from_win32());
        }

        unpremultiply(&mut bgra);
        Ok(Icon {
            width,
            height,
            bgra,
        })
    }
}

fn unpremultiply(bgra: &mut [u8]) {
    for pixel in bgra.as_chunks_mut::<4>().0 {
        let alpha = pixel[3] as u32;
        if alpha > 0 && alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpremultiplies_partial_alpha_only() {
        let mut pixels = [
            100, 50, 0, 128, // half transparent
            10, 20, 30, 255, // opaque: unchanged
            0, 0, 0, 0, // transparent: unchanged
        ];
        unpremultiply(&mut pixels);
        assert_eq!(pixels, [199, 100, 0, 128, 10, 20, 30, 255, 0, 0, 0, 0]);
    }
}
