//! Windows のリリース版はコンソールを持たない（起動のたびに黒い窓を出さないため）。
//! そのままだと、PowerShell などから `malakt-mahoz.exe mcp config` と打っても出力が見えない。

/// 呼び出したターミナルのコンソールにつなぐ。リダイレクトやパイプがあれば、そちらをそのまま使う。
/// Windows 以外では何もしない。
pub fn attach() {
    #[cfg(windows)]
    imp::attach();
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    type Handle = *mut c_void;

    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_INPUT: u32 = -10i32 as u32;
    const STD_OUTPUT: u32 = -11i32 as u32;
    const STD_ERROR: u32 = -12i32 as u32;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const SHARE_READ_WRITE: u32 = 0x3;
    const OPEN_EXISTING: u32 = 3;
    const INVALID: Handle = -1isize as Handle;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process: u32) -> i32;
        fn GetStdHandle(which: u32) -> Handle;
        fn SetStdHandle(which: u32, handle: Handle) -> i32;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *mut c_void,
            disposition: u32,
            flags: u32,
            template: Handle,
        ) -> Handle;
    }

    pub fn attach() {
        // SAFETY: どれも、引数の値だけで動く Win32 の呼び出し。名前は 0 で終わる UTF-16 で、呼び出しの間は生きている
        unsafe {
            // コンソールから呼ばれていない（エクスプローラーから開いた）か、もう持っていれば失敗する
            if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
                return;
            }
            for (which, name) in [
                (STD_INPUT, "CONIN$"),
                (STD_OUTPUT, "CONOUT$"),
                (STD_ERROR, "CONOUT$"),
            ] {
                let now = GetStdHandle(which);
                if !now.is_null() && now != INVALID {
                    // リダイレクトやパイプ（MCP のクライアントなど）
                    continue;
                }
                let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
                let handle = CreateFileW(
                    wide.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    SHARE_READ_WRITE,
                    std::ptr::null_mut(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                );
                if handle != INVALID {
                    SetStdHandle(which, handle);
                }
            }
        }
    }
}
