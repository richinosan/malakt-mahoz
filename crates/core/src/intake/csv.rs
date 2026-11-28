//! CSV（と TSV）を読む。文字コードは UTF-8・UTF-16（BOM つき）・Shift_JIS（CP932）・EUC-JP を見分ける。

/// バイト列を文字列にする。日本語の Redmine や Backlog の CSV は Shift_JIS（CP932）のことがある。
pub fn decode(bytes: &[u8]) -> Result<String, String> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(rest.to_vec())
            .map_err(|_| "の文字コードを読めませんでした".into());
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let (text, _, bad) = encoding_rs::UTF_16LE.decode(bytes);
        if bad {
            return Err("の文字コードを読めませんでした".into());
        }
        return Ok(text.into_owned());
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.to_owned());
    }
    for encoding in [encoding_rs::SHIFT_JIS, encoding_rs::EUC_JP] {
        let (text, bad) = encoding.decode_without_bom_handling(bytes);
        if !bad {
            return Ok(text.into_owned());
        }
    }
    Err("の文字コードを読めませんでした（UTF-8・Shift_JIS・EUC-JP のどれでもありません）".into())
}

/// 区切り文字。1 行目にタブがあってカンマがなければ TSV。
fn delimiter(text: &str) -> char {
    let first = text.lines().next().unwrap_or_default();
    if first.contains('\t') && !first.contains(',') {
        '\t'
    } else if first.contains(';') && !first.contains(',') && !first.contains('\t') {
        ';'
    } else {
        ','
    }
}

/// CSV を行と列にする。引用符の中の改行と `""` を読む。空の行は飛ばす。
pub fn parse(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let sep = delimiter(text);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let push_row = |row: &mut Vec<String>, rows: &mut Vec<Vec<String>>| {
        if !(row.len() == 1 && row[0].is_empty()) && !row.is_empty() {
            rows.push(std::mem::take(row));
        } else {
            row.clear();
        }
    };
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            c if c == sep => row.push(std::mem::take(&mut field)),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                row.push(std::mem::take(&mut field));
                push_row(&mut row, &mut rows);
            }
            '\n' => {
                row.push(std::mem::take(&mut field));
                push_row(&mut row, &mut rows);
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        push_row(&mut row, &mut rows);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_newlines_and_escaped_quotes_are_read() {
        let rows = parse("a,b,c\r\n1,\"x, \"\"y\"\"\nz\",3\n\n4,,\n");
        assert_eq!(
            rows,
            vec![
                vec!["a", "b", "c"],
                vec!["1", "x, \"y\"\nz", "3"],
                vec!["4", "", ""]
            ]
        );
    }

    #[test]
    fn tabs_are_read_as_tsv() {
        assert_eq!(parse("a\tb\n1\t2"), vec![vec!["a", "b"], vec!["1", "2"]]);
    }

    #[test]
    fn japanese_encodings_are_detected() {
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode("題名,担当者\n表示が崩れる,山田");
        assert_eq!(decode(&sjis).unwrap(), "題名,担当者\n表示が崩れる,山田");
        let (euc, _, _) = encoding_rs::EUC_JP.encode("件名\n課題");
        assert_eq!(decode(&euc).unwrap(), "件名\n課題");
        assert_eq!(decode("\u{feff}件名".as_bytes()).unwrap(), "件名");
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("件名".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(
            decode(&utf16).unwrap(),
            "\u{feff}件名".trim_start_matches('\u{feff}')
        );
        assert!(decode(&[0x80, 0xFF, 0xFE, 0x81]).is_err());
    }
}
