use std::sync::mpsc;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LAContext, LAPolicy};

use crate::{Failure, Request};

pub fn verify(request: &Request) -> Result<(), Failure> {
    // Touch ID、なければログインのパスワード
    let policy = LAPolicy::DeviceOwnerAuthentication;
    let context = unsafe { LAContext::new() };
    if unsafe { context.canEvaluatePolicy_error(policy) }.is_err() {
        return Err(Failure::Unavailable);
    }
    let (sender, receiver) = mpsc::channel();
    let reply = RcBlock::new(move |ok: Bool, _error: *mut NSError| {
        // 受け取る側がもういないなら、答えを渡す相手がいない
        let _ = sender.send(ok.as_bool());
    });
    // macOS は「"アプリ名" が <目的> しようとしています」と出す
    let reason = NSString::from_str(&request.action);
    unsafe { context.evaluatePolicy_localizedReason_reply(policy, &reason, &reply) };
    match receiver.recv() {
        Ok(true) => Ok(()),
        Ok(false) => Err(Failure::Denied),
        Err(_) => Err(Failure::Os("macOS から答えが返りませんでした".into())),
    }
}
