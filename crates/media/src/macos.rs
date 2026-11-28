//! macOS の「再生中」（Now Playing）。`MediaRemote.framework` を開いて、操作を送る。

use std::ffi::{c_char, c_int, c_void, CStr};

use crate::{Command, Failure};

const FRAMEWORK: &CStr = c"/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote";
const SEND: &CStr = c"MRMediaRemoteSendCommand";
const RTLD_LAZY: c_int = 1;

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

type SendCommand = unsafe extern "C" fn(command: u32, options: *const c_void) -> bool;

pub fn send(command: Command) -> Result<(), Failure> {
    // MRMediaRemoteCommand の番号
    let code: u32 = match command {
        Command::Play => 0,
        Command::Pause => 1,
        Command::Toggle => 2,
        Command::Next => 4,
        Command::Previous => 5,
    };
    // SAFETY: OS のフレームワークを開き、決まった形の関数を呼ぶ。開いたものは閉じずに使い続ける
    unsafe {
        let handle = dlopen(FRAMEWORK.as_ptr(), RTLD_LAZY);
        if handle.is_null() {
            return Err(Failure::Unsupported);
        }
        let symbol = dlsym(handle, SEND.as_ptr());
        if symbol.is_null() {
            return Err(Failure::Unsupported);
        }
        let send: SendCommand = std::mem::transmute(symbol);
        if send(code, std::ptr::null()) {
            Ok(())
        } else {
            Err(Failure::NoPlayer)
        }
    }
}
