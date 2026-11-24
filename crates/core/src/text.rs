//! OS 間で文字列がずれないようにするための正規化。
//!
//! macOS のファイル名由来の文字列は NFD（濁点が分かれた形）になりやすく、
//! Windows 由来のテキストは CRLF や BOM を含みやすい。保存前にそろえる。

use unicode_normalization::{is_nfc_quick, IsNormalized, UnicodeNormalization};

/// NFC にし、BOM を除き、改行を LF にそろえる。
pub fn normalize(input: &str) -> String {
    let s = input.strip_prefix('\u{feff}').unwrap_or(input);
    let needs_nfc = is_nfc_quick(s.chars()) != IsNormalized::Yes;
    let needs_newline = s.contains('\r');
    if !needs_nfc && !needs_newline {
        return s.to_owned();
    }
    let s = if needs_newline {
        s.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        s.to_owned()
    };
    if needs_nfc {
        s.nfc().collect()
    } else {
        s
    }
}

/// 1 行の名前用。正規化したうえで前後の空白を落とし、改行を空白にする。
pub fn normalize_line(input: &str) -> String {
    normalize(input).replace('\n', " ").trim().to_owned()
}

/// UTF-8 として読む。先頭の BOM は除く。UTF-8 でなければエラー。
pub fn decode_utf8(bytes: &[u8]) -> crate::Result<&str> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    std::str::from_utf8(bytes).map_err(|_| crate::Error::Invalid("UTF-8 ではないデータです".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfd_becomes_nfc() {
        // 「が」を「か」+ 濁点で書いたもの
        assert_eq!(normalize("か\u{3099}"), "が");
    }

    #[test]
    fn crlf_and_cr_become_lf() {
        assert_eq!(normalize("a\r\nb\rc\n"), "a\nb\nc\n");
    }

    #[test]
    fn bom_is_removed() {
        assert_eq!(normalize("\u{feff}abc"), "abc");
        assert_eq!(decode_utf8(&[0xEF, 0xBB, 0xBF, b'a']).unwrap(), "a");
    }

    #[test]
    fn already_normal_is_unchanged() {
        assert_eq!(normalize("準備 タスク"), "準備 タスク");
    }

    #[test]
    fn line_is_trimmed_and_single_line() {
        assert_eq!(normalize_line("  会場\r\n手配  "), "会場 手配");
    }

    #[test]
    fn non_utf8_is_rejected() {
        // Shift_JIS の「あ」
        assert!(decode_utf8(&[0x82, 0xA0]).is_err());
    }
}
