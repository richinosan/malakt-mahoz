//! Windows の、予約した通知。
//!
//! Windows には標準のリマインダーアプリがないので、期限の時刻に出る通知を OS に予約する。
//! 予約は OS が持つので、アプリを閉じていても出る。ワークスペースは通知の見出しになる。

use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::{Error, HSTRING, PCWSTR};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::DateTime;
use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
use windows::UI::Notifications::{
    ScheduledToastNotification, ToastNotificationManager, ToastNotifier,
};

use crate::{toast_alarm_id, toast_id, Item, List, Os, Seen, MAX_TOASTS};

/// 1601 年から 1970 年までの秒数。
const EPOCH_GAP: i64 = 11_644_473_600;

pub struct Toasts {
    notifier: ToastNotifier,
}

fn os(error: Error) -> String {
    error.message()
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 通知の出どころとして、アプリの名前を Windows に登録する。インストーラーのないアプリは、
/// これがないと通知を出せない。
fn register(app_id: &str, app_name: &str) -> Result<(), String> {
    let key = wide(&format!("Software\\Classes\\AppUserModelId\\{app_id}"));
    let value = wide("DisplayName");
    let data = wide(app_name);
    let result = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        )
    };
    result.ok().map_err(os)
}

pub fn open(app_id: &str, app_name: &str) -> Result<Toasts, String> {
    register(app_id, app_name)?;
    let notifier =
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(app_id)).map_err(os)?;
    Ok(Toasts { notifier })
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            // XML に書けない制御文字は落とす
            c if c.is_control() && c != '\n' && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

impl Toasts {
    /// `id` で始まる予約をすべて消す（1 つのタスクの通知は、みな同じ id で始まる）。
    fn unschedule(&self, id: &str) -> windows::core::Result<()> {
        for toast in self.notifier.GetScheduledToastNotifications()? {
            if toast.Id()?.to_string_lossy().starts_with(id) {
                self.notifier.RemoveFromSchedule(&toast)?;
            }
        }
        Ok(())
    }

    fn schedule(&self, id: &str, list: &str, item: &Item, at: i64) -> windows::core::Result<()> {
        let due = item.due.as_deref().unwrap_or_default();
        // 見出し（ワークスペース）ごとに、通知の一覧でまとまる。閉じるまで画面に残す
        let xml = format!(
            "<toast scenario=\"reminder\">\
             <header id=\"{}\" title=\"{}\" arguments=\"\"/>\
             <visual><binding template=\"ToastGeneric\">\
             <text>{}</text><text>期限 {}</text>\
             </binding></visual>\
             <actions><action activationType=\"system\" arguments=\"dismiss\" content=\"\"/></actions>\
             </toast>",
            toast_id(list),
            escape(list),
            escape(&item.title),
            escape(due),
        );
        let document = XmlDocument::new()?;
        document.LoadXml(&HSTRING::from(xml))?;
        let when = DateTime {
            UniversalTime: (at + EPOCH_GAP) * 10_000_000,
        };
        let toast = ScheduledToastNotification::CreateScheduledToastNotification(&document, when)?;
        toast.SetId(&HSTRING::from(id))?;
        self.notifier.AddToSchedule(&toast)
    }
}

impl Os for Toasts {
    /// リストは通知の見出しになるだけなので、名前をそのまま id にする。
    fn save_list(&mut self, _: Option<&str>, list: &List) -> Result<String, String> {
        Ok(list.name.clone())
    }

    fn drop_list(&mut self, _: &str) -> Result<(), String> {
        Ok(())
    }

    fn look(&mut self, _: &str) -> Result<Seen, String> {
        Ok(Seen::Unknown)
    }

    fn save_item(
        &mut self,
        external: Option<&str>,
        list_external: &str,
        item: &Item,
    ) -> Result<String, String> {
        let id = toast_id(&item.task_id);
        self.unschedule(external.unwrap_or(&id)).map_err(os)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        if !item.done {
            // 過ぎた時刻には予約できない
            let ahead = item.alarms.iter().filter(|at| **at > now + 5);
            for (n, at) in ahead.take(MAX_TOASTS).enumerate() {
                self.schedule(&toast_alarm_id(&id, n), list_external, item, *at)
                    .map_err(os)?;
            }
        }
        Ok(id)
    }

    fn drop_item(&mut self, external: &str) -> Result<(), String> {
        self.unschedule(external).map_err(os)
    }
}
