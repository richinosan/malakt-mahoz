use windows::core::{factory, HSTRING};
use windows::Security::Credentials::UI::{
    UserConsentVerificationResult as Outcome, UserConsentVerifier,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::{
    IUserConsentVerifierInterop, RoInitialize, RO_INIT_MULTITHREADED,
};
use windows_future::IAsyncOperation;

use crate::{Failure, Request};

pub fn verify(request: &Request) -> Result<(), Failure> {
    // このスレッドがすでに別の形で初期化されていても、そのまま呼べる
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    let message = HSTRING::from(format!("{}: {}", request.app, request.action));
    let operation: IAsyncOperation<Outcome> = match request.window {
        // ウィンドウを渡すと、Windows Hello のダイアログがそのウィンドウの前に出る
        Some(window) => unsafe {
            factory::<UserConsentVerifier, IUserConsentVerifierInterop>()
                .and_then(|interop| {
                    interop.RequestVerificationForWindowAsync(HWND(window as _), &message)
                })
                .map_err(os)?
        },
        None => UserConsentVerifier::RequestVerificationAsync(&message).map_err(os)?,
    };
    match operation.join().map_err(os)? {
        Outcome::Verified => Ok(()),
        Outcome::DeviceNotPresent | Outcome::NotConfiguredForUser | Outcome::DisabledByPolicy => {
            Err(Failure::Unavailable)
        }
        _ => Err(Failure::Denied),
    }
}

fn os(error: windows::core::Error) -> Failure {
    Failure::Os(error.message())
}
