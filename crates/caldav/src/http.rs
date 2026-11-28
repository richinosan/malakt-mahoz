//! 本物のサーバーとのやり取り（HTTPS、Basic 認証）。呼んだスレッドを止めるので、画面のスレッドからは呼ばない。

use std::time::Duration;

use base64::Engine;
use roxmltree::{Document, Node};

use crate::{Changes, Object, Server, WriteError};

const DAV: &str = "DAV:";
const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
const TIMEOUT: Duration = Duration::from_secs(30);
/// multiget 1 回で読む数。
const FETCH_CHUNK: usize = 100;
const MAX_REDIRECTS: usize = 5;

/// サーバーで見つけたカレンダー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub href: String,
    pub name: String,
    pub read_only: bool,
}

/// アカウントを調べた結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    /// 調べた結果たどり着いたサーバーの URL（以後の基準）。
    pub base: String,
    /// 自分を表す URI（担当者の自分を見分けるのに使う）。
    pub addresses: Vec<String>,
    /// タスクを入れられるカレンダー。
    pub calendars: Vec<Found>,
}

pub struct Http {
    agent: ureq::Agent,
    /// href を URL にするときの基準。
    base: String,
    auth: String,
}

struct Reply {
    status: u16,
    etag: Option<String>,
    location: Option<String>,
    body: String,
}

/// `scheme://host[:port]`。
fn origin(url: &str) -> &str {
    let after = url.find("://").map_or(0, |i| i + 3);
    match url[after..].find('/') {
        Some(i) => &url[..after + i],
        None => url,
    }
}

/// `href` を `base` からの URL にする。
pub fn resolve(base: &str, href: &str) -> String {
    let href = href.trim();
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_owned();
    }
    if href.starts_with('/') {
        return format!("{}{href}", origin(base));
    }
    let dir = match base.rfind('/') {
        Some(i) if i >= origin(base).len() => &base[..=i],
        _ => return format!("{}/{href}", origin(base)),
    };
    format!("{dir}{href}")
}

/// 同じサーバーの URL はパスにする（サーバーが返す href と同じ形にそろえる）。
fn to_href(base: &str, url: &str) -> String {
    let o = origin(base);
    match url.strip_prefix(o) {
        Some(path) if path.starts_with('/') => path.to_owned(),
        _ => url.to_owned(),
    }
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn child<'a>(node: Node<'a, 'a>, ns: &str, name: &str) -> Option<Node<'a, 'a>> {
    node.children().find(|c| {
        c.is_element() && c.tag_name().name() == name && c.tag_name().namespace() == Some(ns)
    })
}

fn children<'a>(
    node: Node<'a, 'a>,
    ns: &'a str,
    name: &'a str,
) -> impl Iterator<Item = Node<'a, 'a>> + 'a {
    node.children().filter(move |c| {
        c.is_element() && c.tag_name().name() == name && c.tag_name().namespace() == Some(ns)
    })
}

fn text(node: Node) -> String {
    node.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect::<String>()
        .trim()
        .to_owned()
}

/// `HTTP/1.1 200 OK` の 200。
fn status_code(node: Option<Node>) -> Option<u16> {
    let t = text(node?);
    t.split_whitespace().nth(1)?.parse().ok()
}

/// multistatus の 1 件。
struct Entry<'a> {
    href: String,
    status: Option<u16>,
    /// 200 で返った prop の中身。
    props: Vec<Node<'a, 'a>>,
}

impl<'a> Entry<'a> {
    fn prop(&self, ns: &str, name: &str) -> Option<Node<'a, 'a>> {
        self.props.iter().find_map(|p| child(*p, ns, name))
    }
}

fn entries<'a>(doc: &'a Document<'a>) -> Vec<Entry<'a>> {
    let root = doc.root_element();
    children(root, DAV, "response")
        .map(|r| {
            let href = child(r, DAV, "href").map(text).unwrap_or_default();
            let status = status_code(child(r, DAV, "status"));
            let props = children(r, DAV, "propstat")
                .filter(|ps| {
                    status_code(child(*ps, DAV, "status")).is_none_or(|s| (200..300).contains(&s))
                })
                .filter_map(|ps| child(ps, DAV, "prop"))
                .collect();
            Entry {
                href,
                status,
                props,
            }
        })
        .collect()
}

fn parse_xml(body: &str) -> Result<Document<'_>, String> {
    Document::parse(body).map_err(|e| format!("サーバーの返事を読めませんでした（{e}）"))
}

fn describe(status: u16) -> String {
    match status {
        401 => "ユーザー名かパスワードが違います（401）".into(),
        403 => "このアカウントでは使えません（403）".into(),
        404 => "URL が見つかりません（404）".into(),
        s => format!("サーバーがエラーを返しました（{s}）"),
    }
}

impl Http {
    pub fn new(base: &str, username: &str, password: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .allow_non_standard_methods(true)
            .max_redirects(0)
            .timeout_global(Some(TIMEOUT))
            .user_agent(concat!("malakt-mahoz/", env!("CARGO_PKG_VERSION")))
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .build()
            .into();
        let token =
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
        Http {
            agent,
            base: base.trim().to_owned(),
            auth: format!("Basic {token}"),
        }
    }

    fn url(&self, href: &str) -> String {
        resolve(&self.base, href)
    }

    /// 送る。リダイレクトはメソッドと中身を変えずにたどる。最後にたどり着いた URL も返す。
    fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> Result<(Reply, String), String> {
        let mut url = url.to_owned();
        for _ in 0..=MAX_REDIRECTS {
            let mut builder = ureq::http::Request::builder()
                .method(
                    ureq::http::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?,
                )
                .uri(&url)
                .header("Authorization", &self.auth);
            for (name, value) in headers {
                builder = builder.header(*name, *value);
            }
            let result = match body {
                Some(b) => self
                    .agent
                    .run(builder.body(b.to_owned()).map_err(|e| e.to_string())?),
                None => self.agent.run(builder.body(()).map_err(|e| e.to_string())?),
            };
            let mut response =
                result.map_err(|e| format!("{} に接続できませんでした（{e}）", origin(&url)))?;
            let header = |name: &str| {
                response
                    .headers()
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
            };
            let reply = Reply {
                status: response.status().as_u16(),
                etag: header("etag"),
                location: header("location"),
                body: String::new(),
            };
            if matches!(reply.status, 301 | 302 | 303 | 307 | 308) {
                if let Some(location) = reply.location {
                    url = resolve(&url, &location);
                    continue;
                }
            }
            let body = response
                .body_mut()
                .with_config()
                .limit(64 * 1024 * 1024)
                .read_to_string()
                .map_err(|e| format!("サーバーの返事を読めませんでした（{e}）"))?;
            return Ok((Reply { body, ..reply }, url));
        }
        Err("リダイレクトが多すぎます".into())
    }

    fn propfind(&self, url: &str, depth: &str, props: &str) -> Result<(Reply, String), String> {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <d:propfind xmlns:d=\"DAV:\" xmlns:c=\"{CALDAV}\" xmlns:cs=\"http://calendarserver.org/ns/\">\
             <d:prop>{props}</d:prop></d:propfind>"
        );
        self.send(
            "PROPFIND",
            url,
            &[
                ("Depth", depth),
                ("Content-Type", "application/xml; charset=utf-8"),
            ],
            Some(&body),
        )
    }

    fn report(&self, href: &str, depth: &str, body: &str) -> Result<Reply, String> {
        let (reply, _) = self.send(
            "REPORT",
            &self.url(href),
            &[
                ("Depth", depth),
                ("Content-Type", "application/xml; charset=utf-8"),
            ],
            Some(body),
        )?;
        Ok(reply)
    }

    /// アカウントを調べる。`url` は、サーバーの URL か、CalDAV の URL。
    pub fn discover(url: &str, username: &str, password: &str) -> Result<Discovery, String> {
        let url = url.trim();
        let url = if url.contains("://") {
            url.to_owned()
        } else {
            format!("https://{url}")
        };
        let mut http = Http::new(&url, username, password);
        // 1. 自分のプリンシパル。入れた URL で見つからなければ /.well-known/caldav（RFC 6764）
        let mut principal = None;
        let mut last_error = None;
        for candidate in [url.clone(), format!("{}/.well-known/caldav", origin(&url))] {
            let (reply, at) = match http.propfind(&candidate, "0", "<d:current-user-principal/>") {
                Ok(r) => r,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };
            if reply.status == 401 {
                return Err(describe(401));
            }
            if reply.status != 207 {
                last_error = Some(describe(reply.status));
                continue;
            }
            let doc = parse_xml(&reply.body)?;
            let found = entries(&doc).iter().find_map(|e| {
                e.prop(DAV, "current-user-principal")
                    .and_then(|p| child(p, DAV, "href"))
                    .map(text)
            });
            if let Some(href) = found {
                http.base = at.clone();
                principal = Some(resolve(&at, &href));
                break;
            }
            last_error = Some("CalDAV のサーバーではないようです".into());
        }
        let principal = principal.ok_or_else(|| {
            last_error.unwrap_or_else(|| "CalDAV のサーバーが見つかりません".into())
        })?;
        // 2. カレンダーの置き場と、自分の URI
        let (reply, _) = http.propfind(
            &principal,
            "0",
            "<c:calendar-home-set/><c:calendar-user-address-set/>",
        )?;
        if reply.status != 207 {
            return Err(describe(reply.status));
        }
        let doc = parse_xml(&reply.body)?;
        let list = entries(&doc);
        let home = list
            .iter()
            .find_map(|e| {
                e.prop(CALDAV, "calendar-home-set")
                    .and_then(|p| child(p, DAV, "href"))
                    .map(text)
            })
            .ok_or("カレンダーの置き場が見つかりません")?;
        let mut addresses: Vec<String> = list
            .iter()
            .filter_map(|e| e.prop(CALDAV, "calendar-user-address-set"))
            .flat_map(|p| children(p, DAV, "href").map(text).collect::<Vec<_>>())
            .filter(|a| !a.is_empty())
            .collect();
        addresses.push(principal.clone());
        // 3. カレンダーの一覧
        let home = resolve(&principal, &home);
        let (reply, _) = http.propfind(
            &home,
            "1",
            "<d:resourcetype/><d:displayname/><c:supported-calendar-component-set/><d:current-user-privilege-set/>",
        )?;
        if reply.status != 207 {
            return Err(describe(reply.status));
        }
        let doc = parse_xml(&reply.body)?;
        let mut calendars = Vec::new();
        for e in entries(&doc) {
            let is_calendar = e
                .prop(DAV, "resourcetype")
                .is_some_and(|r| child(r, CALDAV, "calendar").is_some());
            if !is_calendar {
                continue;
            }
            // 入れられるものの一覧がなければ、何でも入れられる
            let todo = e
                .prop(CALDAV, "supported-calendar-component-set")
                .is_none_or(|set| {
                    children(set, CALDAV, "comp").any(|c| {
                        c.attribute("name")
                            .is_some_and(|n| n.eq_ignore_ascii_case("VTODO"))
                    })
                });
            if !todo {
                continue;
            }
            let read_only = e
                .prop(DAV, "current-user-privilege-set")
                .is_some_and(|set| {
                    !set.descendants().any(|n| {
                        n.is_element()
                            && n.tag_name().namespace() == Some(DAV)
                            && matches!(n.tag_name().name(), "write" | "write-content" | "all")
                    })
                });
            let href = to_href(&http.base, &resolve(&home, &e.href));
            let name = e
                .prop(DAV, "displayname")
                .map(text)
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| {
                    href.trim_end_matches('/')
                        .rsplit('/')
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                });
            calendars.push(Found {
                href,
                name,
                read_only,
            });
        }
        Ok(Discovery {
            base: http.base,
            addresses,
            calendars,
        })
    }

    fn list_all(&self, calendar: &str) -> Result<Vec<(String, String)>, String> {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <c:calendar-query xmlns:d=\"DAV:\" xmlns:c=\"{CALDAV}\">\
             <d:prop><d:getetag/></d:prop>\
             <c:filter><c:comp-filter name=\"VCALENDAR\"><c:comp-filter name=\"VTODO\"/></c:comp-filter></c:filter>\
             </c:calendar-query>"
        );
        let reply = self.report(calendar, "1", &body)?;
        if reply.status != 207 {
            return Err(describe(reply.status));
        }
        let doc = parse_xml(&reply.body)?;
        Ok(entries(&doc)
            .iter()
            .filter_map(|e| {
                let etag = e.prop(DAV, "getetag").map(text)?;
                Some((to_href(&self.base, &e.href), etag))
            })
            .collect())
    }
}

impl Server for Http {
    fn changes(&mut self, calendar: &str, token: Option<&str>) -> Result<Changes, String> {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <d:sync-collection xmlns:d=\"DAV:\">\
             <d:sync-token>{}</d:sync-token><d:sync-level>1</d:sync-level>\
             <d:prop><d:getetag/></d:prop></d:sync-collection>",
            escape_xml(token.unwrap_or_default())
        );
        let reply = self.report(calendar, "1", &body)?;
        match reply.status {
            207 => {}
            401 => return Err(describe(401)),
            // 印が古くなった（RFC 6578 3.2）。最初から読み直す
            403 | 409 if token.is_some() && reply.body.contains("valid-sync-token") => {
                return self.changes(calendar, None);
            }
            // 差分を出せないサーバー
            _ => {
                return Ok(Changes::Full {
                    all: self.list_all(calendar)?,
                })
            }
        }
        let doc = parse_xml(&reply.body)?;
        let new_token = child(doc.root_element(), DAV, "sync-token").map(text);
        let Some(new_token) = new_token.filter(|t| !t.is_empty()) else {
            return Ok(Changes::Full {
                all: self.list_all(calendar)?,
            });
        };
        let mut changed = Vec::new();
        let mut removed = Vec::new();
        for e in entries(&doc) {
            let href = to_href(&self.base, &e.href);
            if e.status == Some(404) {
                removed.push(href);
            } else if let Some(etag) = e.prop(DAV, "getetag").map(text) {
                changed.push((href, etag));
            }
        }
        Ok(Changes::Delta {
            token: new_token,
            changed,
            removed,
        })
    }

    fn fetch(&mut self, calendar: &str, hrefs: &[String]) -> Result<Vec<Object>, String> {
        let mut out = Vec::new();
        for chunk in hrefs.chunks(FETCH_CHUNK) {
            let list: String = chunk
                .iter()
                .map(|h| format!("<d:href>{}</d:href>", escape_xml(h)))
                .collect();
            let body = format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
                 <c:calendar-multiget xmlns:d=\"DAV:\" xmlns:c=\"{CALDAV}\">\
                 <d:prop><d:getetag/><c:calendar-data/></d:prop>{list}</c:calendar-multiget>"
            );
            let reply = self.report(calendar, "1", &body)?;
            if reply.status != 207 {
                return Err(describe(reply.status));
            }
            let doc = parse_xml(&reply.body)?;
            for e in entries(&doc) {
                let (Some(etag), Some(data)) = (
                    e.prop(DAV, "getetag").map(text),
                    e.prop(CALDAV, "calendar-data").map(text),
                ) else {
                    continue;
                };
                out.push(Object {
                    href: to_href(&self.base, &e.href),
                    etag,
                    ics: data,
                });
            }
        }
        Ok(out)
    }

    fn get(&mut self, href: &str) -> Result<Option<Object>, String> {
        let (reply, _) = self.send("GET", &self.url(href), &[], None)?;
        match reply.status {
            404 | 410 => Ok(None),
            200 => Ok(Some(Object {
                href: href.to_owned(),
                etag: reply.etag.unwrap_or_default(),
                ics: reply.body,
            })),
            s => Err(describe(s)),
        }
    }

    fn put(
        &mut self,
        href: &str,
        ics: &str,
        etag: Option<&str>,
    ) -> Result<Option<String>, WriteError> {
        let quoted;
        let condition = match etag {
            Some(e) => {
                quoted = if e.starts_with('"') || e.starts_with("W/") {
                    e.to_owned()
                } else {
                    format!("\"{e}\"")
                };
                ("If-Match", quoted.as_str())
            }
            None => ("If-None-Match", "*"),
        };
        let (reply, _) = self
            .send(
                "PUT",
                &self.url(href),
                &[condition, ("Content-Type", "text/calendar; charset=utf-8")],
                Some(ics),
            )
            .map_err(WriteError::Other)?;
        match reply.status {
            200..=299 => Ok(reply.etag),
            412 => Err(WriteError::Conflict),
            403 => Err(WriteError::Forbidden(
                reply
                    .body
                    .chars()
                    .take(200)
                    .collect::<String>()
                    .trim()
                    .to_owned(),
            )),
            404 | 410 => Err(WriteError::Gone),
            s => Err(WriteError::Other(describe(s))),
        }
    }

    fn delete(&mut self, href: &str) -> Result<(), WriteError> {
        let (reply, _) = self
            .send("DELETE", &self.url(href), &[], None)
            .map_err(WriteError::Other)?;
        match reply.status {
            200..=299 => Ok(()),
            404 | 410 => Err(WriteError::Gone),
            412 => Err(WriteError::Conflict),
            403 => Err(WriteError::Forbidden(String::new())),
            s => Err(WriteError::Other(describe(s))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_hrefs() {
        assert_eq!(
            resolve("https://a.example/dav/", "/x/y"),
            "https://a.example/x/y"
        );
        assert_eq!(
            resolve("https://a.example/dav/", "y.ics"),
            "https://a.example/dav/y.ics"
        );
        assert_eq!(resolve("https://a.example", "y"), "https://a.example/y");
        assert_eq!(
            resolve("https://a.example/dav/", "https://b.example/z"),
            "https://b.example/z"
        );
        assert_eq!(
            to_href("https://a.example/dav/", "https://a.example/x/"),
            "/x/"
        );
        assert_eq!(
            to_href("https://a.example/dav/", "https://b.example/x/"),
            "https://b.example/x/"
        );
    }

    #[test]
    fn reads_a_multistatus() {
        let body = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:response><d:href>/dav/c/1.ics</d:href>
    <d:propstat><d:prop><d:getetag>"e1"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
    <d:propstat><d:prop><c:calendar-data/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
  </d:response>
  <d:response><d:href>/dav/c/2.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>
  <d:sync-token>t2</d:sync-token>
</d:multistatus>"#;
        let doc = parse_xml(body).unwrap();
        let list = entries(&doc);
        assert_eq!(list.len(), 2);
        assert_eq!(
            list[0].prop(DAV, "getetag").map(text).as_deref(),
            Some("\"e1\"")
        );
        assert!(list[0].prop(CALDAV, "calendar-data").is_none());
        assert_eq!(list[1].status, Some(404));
        assert_eq!(
            child(doc.root_element(), DAV, "sync-token")
                .map(text)
                .as_deref(),
            Some("t2")
        );
    }
}
