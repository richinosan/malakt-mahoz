//! コメントの `@名前` を読む。

use crate::model::{Id, Person, MAX_MENTIONS};

/// メンションで書く名前。表示名から空白を除いたもの。
pub fn handle(name: &str) -> String {
    name.chars().filter(|c| !c.is_whitespace()).collect()
}

/// 本文で `@名前` と書かれた人を、出てきた順に返す。
///
/// 登録されていない名前は無視する。同じ人は 1 回だけ。名前が別の名前の前半に
/// なっているとき（「佐藤」と「佐藤花子」）は、長いほうを優先する。
pub fn find(body: &str, people: &[Person]) -> Vec<Id> {
    let mut handles: Vec<(String, &Id)> = people
        .iter()
        .map(|p| (handle(&p.name), &p.id))
        .filter(|(h, _)| !h.is_empty())
        .collect();
    handles.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    let mut found: Vec<Id> = Vec::new();
    for (at, _) in body.match_indices('@') {
        let rest = &body[at + 1..];
        if let Some((_, id)) = handles.iter().find(|(h, _)| rest.starts_with(h.as_str())) {
            if !found.contains(id) && found.len() < MAX_MENTIONS {
                found.push((*id).clone());
            }
        }
    }
    found
}

/// 入力の末尾が書きかけのメンション（`@さと`）なら、`@` のあとの文字列を返す。
pub fn pending(text: &str) -> Option<&str> {
    let at = text.rfind('@')?;
    let query = &text[at + 1..];
    let starts_word = text[..at]
        .chars()
        .next_back()
        .is_none_or(|c| c.is_whitespace());
    (starts_word && !query.chars().any(char::is_whitespace)).then_some(query)
}

/// 書きかけのメンションに合う人。名前の前方一致、名前の順。
pub fn candidates<'a>(query: &str, people: &'a [Person]) -> Vec<&'a Person> {
    let query = query.to_lowercase();
    let mut hits: Vec<&Person> = people
        .iter()
        .filter(|p| handle(&p.name).to_lowercase().starts_with(&query))
        .collect();
    hits.sort_by(|a, b| a.name.cmp(&b.name));
    hits
}

/// 書きかけのメンションを、選んだ人の名前で置き換える。
pub fn complete(text: &str, person: &Person) -> String {
    match text.rfind('@') {
        Some(at) => format!("{}@{} ", &text[..at], handle(&person.name)),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{candidates, complete, find, handle, pending};
    use crate::model::Person;

    fn person(id: &str, name: &str) -> Person {
        Person {
            id: id.into(),
            name: name.into(),
            is_self: false,
            note: String::new(),
            created_at: 0,
            updated_at: 0,
            workspace_ids: Vec::new(),
        }
    }

    fn people() -> Vec<Person> {
        vec![
            person("a", "佐藤"),
            person("b", "佐藤 花子"),
            person("c", "Alice"),
        ]
    }

    #[test]
    fn handle_drops_spaces() {
        assert_eq!(handle("佐藤 花子"), "佐藤花子");
        assert_eq!(handle(" A\u{3000}B "), "AB");
    }

    #[test]
    fn finds_known_names_once_in_order() {
        let people = people();
        assert_eq!(
            find("@Alice と @佐藤 にお願い。@Alice", &people),
            ["c", "a"]
        );
        // 長い名前を優先する
        assert_eq!(find("@佐藤花子 さん", &people), ["b"]);
        // 知らない名前、@ だけ
        assert!(find("@田中 @ ただの文", &people).is_empty());
        assert!(find("", &people).is_empty());
    }

    #[test]
    fn mentions_are_capped() {
        let many: Vec<Person> = (0..60)
            .map(|i| person(&format!("p{i:02}"), &format!("人{i:02}番")))
            .collect();
        let body: String = many.iter().map(|p| format!("@{} ", p.name)).collect();
        assert_eq!(find(&body, &many).len(), super::MAX_MENTIONS);
    }

    #[test]
    fn pending_mention_at_the_end() {
        assert_eq!(pending("確認 @さと"), Some("さと"));
        assert_eq!(pending("@"), Some(""));
        assert_eq!(pending("確認 @佐藤 お願い"), None);
        assert_eq!(pending("a@b"), None);
        assert_eq!(pending("なし"), None);
    }

    #[test]
    fn candidates_match_by_prefix() {
        let people = people();
        let names = |q| {
            candidates(q, &people)
                .into_iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(names("佐"), ["佐藤", "佐藤 花子"]);
        assert_eq!(names("ali"), ["Alice"]);
        assert_eq!(names("").len(), 3);
        assert!(names("田").is_empty());
    }

    #[test]
    fn complete_replaces_the_pending_part() {
        let people = people();
        assert_eq!(complete("確認 @さ", &people[1]), "確認 @佐藤花子 ");
        assert_eq!(complete("なし", &people[0]), "なし");
    }
}
