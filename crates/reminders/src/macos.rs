//! macOS の「リマインダー」（EventKit）。

use std::sync::mpsc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObjectProtocol};
use objc2::sel;
use objc2_event_kit::{
    EKAlarm, EKAuthorizationStatus, EKCalendar, EKEntityType, EKEventStore, EKReminder,
};
use objc2_foundation::{NSArray, NSDate, NSDateComponents, NSError, NSString};

use crate::{Item, List, Os, Seen};

const DENIED: &str = "リマインダーへのアクセスが許可されていません。システム設定の「プライバシーとセキュリティ」>「リマインダー」で許可してください";

pub struct Reminders {
    store: Retained<EKEventStore>,
}

/// 「リマインダー」を開く。最初の 1 回は、macOS がアクセスの許可を聞く。
pub fn open() -> Result<Reminders, String> {
    let store = unsafe { EKEventStore::new() };
    let status = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Reminder) };
    if status == EKAuthorizationStatus::FullAccess {
        return Ok(Reminders { store });
    }
    if status != EKAuthorizationStatus::NotDetermined {
        return Err(DENIED.into());
    }
    let (sender, receiver) = mpsc::channel();
    let reply = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
        // 受け取る側がもういないなら、答えを渡す相手がいない
        let _ = sender.send(granted.as_bool());
    });
    unsafe {
        // macOS 14 からは、読み書きの許可を別の呼び出しで求める
        if store.respondsToSelector(sel!(requestFullAccessToRemindersWithCompletion:)) {
            store.requestFullAccessToRemindersWithCompletion(RcBlock::as_ptr(&reply));
        } else {
            #[allow(deprecated)]
            store.requestAccessToEntityType_completion(
                EKEntityType::Reminder,
                RcBlock::as_ptr(&reply),
            );
        }
    }
    match receiver.recv() {
        Ok(true) => Ok(Reminders { store }),
        Ok(false) => Err(DENIED.into()),
        Err(_) => Err("macOS から答えが返りませんでした".into()),
    }
}

fn text(error: Retained<NSError>) -> String {
    error.localizedDescription().to_string()
}

impl Reminders {
    fn calendar(&self, external: &str) -> Option<Retained<EKCalendar>> {
        unsafe {
            self.store
                .calendarWithIdentifier(&NSString::from_str(external))
        }
    }

    fn reminder(&self, external: &str) -> Option<Retained<EKReminder>> {
        unsafe {
            self.store
                .calendarItemWithIdentifier(&NSString::from_str(external))
        }
        .and_then(|item| item.downcast::<EKReminder>().ok())
    }

    /// リストに入っているリマインダーの数。
    fn count_in(&self, calendar: &EKCalendar) -> Result<usize, String> {
        let calendars = NSArray::from_slice(&[calendar]);
        let predicate = unsafe {
            self.store
                .predicateForRemindersInCalendars(Some(&calendars))
        };
        let (sender, receiver) = mpsc::channel();
        let reply = RcBlock::new(move |found: *mut NSArray<EKReminder>| {
            let count = unsafe { found.as_ref() }.map(|found| found.count());
            // 受け取る側がもういないなら、答えを渡す相手がいない
            let _ = sender.send(count);
        });
        let _request = unsafe {
            self.store
                .fetchRemindersMatchingPredicate_completion(&predicate, &reply)
        };
        match receiver.recv() {
            Ok(Some(count)) => Ok(count),
            _ => Err("リストの中身を読めませんでした".into()),
        }
    }
}

impl Os for Reminders {
    fn save_list(&mut self, external: Option<&str>, list: &List) -> Result<String, String> {
        let name = NSString::from_str(&list.name);
        let calendar = match external.and_then(|id| self.calendar(id)) {
            Some(calendar) => {
                if unsafe { calendar.title() }.to_string() == list.name {
                    return Ok(unsafe { calendar.calendarIdentifier() }.to_string());
                }
                calendar
            }
            None => unsafe {
                let calendar = EKCalendar::calendarForEntityType_eventStore(
                    EKEntityType::Reminder,
                    &self.store,
                );
                // 新しいリストは、標準のリストと同じ置き場所（このMac 内 / iCloud）に作る
                let source = self
                    .store
                    .defaultCalendarForNewReminders()
                    .and_then(|c| c.source());
                let Some(source) = source else {
                    return Err("リマインダーのリストの置き場所が見つかりません".into());
                };
                calendar.setSource(Some(&source));
                calendar
            },
        };
        unsafe {
            calendar.setTitle(&name);
            self.store
                .saveCalendar_commit_error(&calendar, true)
                .map_err(text)?;
            Ok(calendar.calendarIdentifier().to_string())
        }
    }

    fn drop_list(&mut self, external: &str) -> Result<(), String> {
        let Some(calendar) = self.calendar(external) else {
            return Ok(());
        };
        // 利用者が自分で足したリマインダーが入っているリストは残す
        if self.count_in(&calendar)? > 0 {
            return Ok(());
        }
        unsafe { self.store.removeCalendar_commit_error(&calendar, true) }.map_err(text)
    }

    fn look(&mut self, external: &str) -> Result<Seen, String> {
        Ok(match self.reminder(external) {
            None => Seen::Missing,
            Some(reminder) if unsafe { reminder.isCompleted() } => Seen::Done,
            Some(_) => Seen::Open,
        })
    }

    fn save_item(
        &mut self,
        external: Option<&str>,
        list_external: &str,
        item: &Item,
    ) -> Result<String, String> {
        let Some(calendar) = self.calendar(list_external) else {
            return Err("リストが見つかりません".into());
        };
        let reminder = match external.and_then(|id| self.reminder(id)) {
            Some(reminder) => reminder,
            None => unsafe { EKReminder::reminderWithEventStore(&self.store) },
        };
        unsafe {
            reminder.setCalendar(Some(&calendar));
            reminder.setTitle(Some(&NSString::from_str(&item.title)));
            let notes = (!item.notes.is_empty()).then(|| NSString::from_str(&item.notes));
            reminder.setNotes(notes.as_deref());
            reminder.setPriority(usize::from(item.priority));
            // 期限と、アプリの設定で決めた知らせる時刻
            reminder.setAlarms(None);
            match item.due_parts() {
                Some((year, month, day, time)) => {
                    let due = NSDateComponents::new();
                    due.setYear(year as isize);
                    due.setMonth(month as isize);
                    due.setDay(day as isize);
                    if let Some((hour, minute)) = time {
                        due.setHour(hour as isize);
                        due.setMinute(minute as isize);
                    }
                    reminder.setDueDateComponents(Some(&due));
                    for at in &item.alarms {
                        let at = NSDate::dateWithTimeIntervalSince1970(*at as f64);
                        reminder.addAlarm(&EKAlarm::alarmWithAbsoluteDate(&at));
                    }
                }
                None => reminder.setDueDateComponents(None),
            }
            if reminder.isCompleted() != item.done {
                reminder.setCompleted(item.done);
            }
            self.store
                .saveReminder_commit_error(&reminder, true)
                .map_err(text)?;
            Ok(reminder.calendarItemIdentifier().to_string())
        }
    }

    fn drop_item(&mut self, external: &str) -> Result<(), String> {
        let Some(reminder) = self.reminder(external) else {
            return Ok(());
        };
        unsafe { self.store.removeReminder_commit_error(&reminder, true) }.map_err(text)
    }
}
