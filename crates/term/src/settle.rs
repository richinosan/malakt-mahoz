//! シェルが終わる直前の出力を落とさないための PTY の包み。
//!
//! alacritty は、PTY から読んだ分を、端末の画面のロックが取れてから解釈する。
//! ロックを待つ間にもう一度読み、シェルが閉じたあとの PTY が `EIO` を返すと、
//! 読み終えていた分を解釈しないまま捨てる。画面を描いている最中にシェルが終わると、
//! 最後の出力（`echo last-words; exit 3` の行）が出ないまま消える。
//! Linux では、閉じたあとの `EIO` を「もう読むものがない」（0 バイト）として返し、
//! 読み終えていた分をロックが取れてから解釈させる。

use std::io;
use std::sync::Arc;

use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::tty::{self, ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event, PollMode, Poller};

pub struct Pty {
    inner: tty::Pty,
    #[cfg(target_os = "linux")]
    reader: Reader,
}

impl Pty {
    pub fn new(inner: tty::Pty) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let mut inner = inner;
            // 同じ PTY を指す 2 つ目の口。登録は元の口のまま、読むのはこちらで行う
            let reader = Reader(inner.reader().try_clone()?);
            Ok(Self { inner, reader })
        }
        #[cfg(not(target_os = "linux"))]
        Ok(Self { inner })
    }
}

/// 閉じたあとの `EIO` を、読み終わり（0 バイト）として返す読み口。
#[cfg(target_os = "linux")]
pub struct Reader(std::fs::File);

#[cfg(target_os = "linux")]
impl io::Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.read(buf) {
            Err(err) if err.raw_os_error() == Some(libc::EIO) => Ok(0),
            other => other,
        }
    }
}

impl EventedReadWrite for Pty {
    #[cfg(target_os = "linux")]
    type Reader = Reader;
    #[cfg(not(target_os = "linux"))]
    type Reader = <tty::Pty as EventedReadWrite>::Reader;
    type Writer = <tty::Pty as EventedReadWrite>::Writer;

    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        event: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        // SAFETY: 包んでいる PTY の約束をそのまま引き継ぐ
        unsafe { self.inner.register(poll, event, mode) }
    }

    fn reregister(&mut self, poll: &Arc<Poller>, event: Event, mode: PollMode) -> io::Result<()> {
        self.inner.reregister(poll, event, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        self.inner.deregister(poll)
    }

    fn reader(&mut self) -> &mut Self::Reader {
        #[cfg(target_os = "linux")]
        return &mut self.reader;
        #[cfg(not(target_os = "linux"))]
        return self.inner.reader();
    }

    fn writer(&mut self) -> &mut Self::Writer {
        self.inner.writer()
    }
}

impl EventedPty for Pty {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.inner.next_child_event()
    }
}

impl OnResize for Pty {
    fn on_resize(&mut self, size: WindowSize) {
        self.inner.on_resize(size);
    }
}
