//! macOS の「再生中」（Now Playing）。`MediaRemote.framework` を開いて、操作を送り、曲を読む。

use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::mpsc;
use std::time::Duration;

use block2::{Block, RcBlock};

use crate::{Command, Failure, NowPlaying, MAX_ARTWORK};

const FRAMEWORK: &CStr = c"/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote";
const SEND: &CStr = c"MRMediaRemoteSendCommand";
const INFO: &CStr = c"MRMediaRemoteGetNowPlayingInfo";
const RTLD_LAZY: c_int = 1;
/// kCFStringEncodingUTF8
const UTF8: u32 = 0x0800_0100;
/// kCFNumberDoubleType
const DOUBLE: isize = 13;

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        s: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFRelease(cf: *const c_void);
    fn CFGetTypeID(cf: *const c_void) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFDataGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
    fn CFStringGetLength(s: *const c_void) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(s: *const c_void, buf: *mut c_char, size: isize, encoding: u32) -> bool;
    fn CFDataGetLength(data: *const c_void) -> isize;
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFNumberGetValue(number: *const c_void, kind: isize, out: *mut c_void) -> bool;
}

type GetInfo = unsafe extern "C" fn(queue: *mut c_void, block: &Block<dyn Fn(*const c_void)>);

/// MediaRemote の関数を探す。
fn symbol(name: &CStr) -> Result<*mut c_void, Failure> {
    // SAFETY: OS のフレームワークを開くだけ。開いたものは閉じずに使い続ける
    unsafe {
        let handle = dlopen(FRAMEWORK.as_ptr(), RTLD_LAZY);
        if handle.is_null() {
            return Err(Failure::Unsupported);
        }
        let symbol = dlsym(handle, name.as_ptr());
        if symbol.is_null() {
            return Err(Failure::Unsupported);
        }
        Ok(symbol)
    }
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
    let symbol = symbol(SEND)?;
    // SAFETY: MRMediaRemoteSendCommand は、この形の関数
    unsafe {
        let send: SendCommand = std::mem::transmute(symbol);
        if send(code, std::ptr::null()) {
            Ok(())
        } else {
            Err(Failure::NoPlayer)
        }
    }
}

/// 辞書の値を読む。`key` は CFString の名前。
///
/// # Safety
/// `dict` は生きている CFDictionary。
unsafe fn value(dict: *const c_void, key: &CStr) -> *const c_void {
    let key = CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), UTF8);
    if key.is_null() {
        return std::ptr::null();
    }
    let value = CFDictionaryGetValue(dict, key);
    CFRelease(key);
    value
}

/// # Safety
/// `dict` は生きている CFDictionary。
unsafe fn text(dict: *const c_void, key: &CStr) -> String {
    let s = value(dict, key);
    if s.is_null() || CFGetTypeID(s) != CFStringGetTypeID() {
        return String::new();
    }
    let size = CFStringGetMaximumSizeForEncoding(CFStringGetLength(s), UTF8) + 1;
    let mut buf = vec![0 as c_char; size.max(1) as usize];
    if !CFStringGetCString(s, buf.as_mut_ptr(), size, UTF8) {
        return String::new();
    }
    CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
}

/// # Safety
/// `dict` は生きている CFDictionary。
unsafe fn read(dict: *const c_void) -> Option<NowPlaying> {
    if dict.is_null() {
        return None;
    }
    let title = text(dict, c"kMRMediaRemoteNowPlayingInfoTitle");
    if title.is_empty() {
        return None;
    }
    let rate = value(dict, c"kMRMediaRemoteNowPlayingInfoPlaybackRate");
    let mut speed = 0.0f64;
    if !rate.is_null() && CFGetTypeID(rate) == CFNumberGetTypeID() {
        CFNumberGetValue(rate, DOUBLE, (&mut speed as *mut f64).cast());
    }
    let data = value(dict, c"kMRMediaRemoteNowPlayingInfoArtworkData");
    let artwork = (!data.is_null() && CFGetTypeID(data) == CFDataGetTypeID())
        .then(|| (CFDataGetBytePtr(data), CFDataGetLength(data) as usize))
        .filter(|(ptr, len)| !ptr.is_null() && *len > 0 && *len <= MAX_ARTWORK)
        .map(|(ptr, len)| std::slice::from_raw_parts(ptr, len).to_vec());
    Some(NowPlaying {
        title,
        artist: text(dict, c"kMRMediaRemoteNowPlayingInfoArtist"),
        album: text(dict, c"kMRMediaRemoteNowPlayingInfoAlbum"),
        playing: speed > 0.0,
        artwork,
    })
}

pub fn now_playing() -> Result<Option<NowPlaying>, Failure> {
    let symbol = symbol(INFO)?;
    let (tx, rx) = mpsc::channel();
    // 辞書は呼ばれている間だけ生きているので、その場で読み写す
    let block = RcBlock::new(move |dict: *const c_void| {
        // SAFETY: MediaRemote が渡す CFDictionary（なければ NULL）
        let playing = unsafe { read(dict) };
        // 待つのをやめたあとに届いた答えは、捨ててよい
        let _ = tx.send(playing);
    });
    // SAFETY: MRMediaRemoteGetNowPlayingInfo は、この形の関数。ブロックは向こうで写して持つ
    unsafe {
        let get: GetInfo = std::mem::transmute(symbol);
        get(dispatch_get_global_queue(0, 0), &block);
    }
    rx.recv_timeout(Duration::from_secs(3))
        .map_err(|_| Failure::Os("再生中の曲の問い合わせに答えがありません".into()))
}
