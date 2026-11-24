use std::io::ErrorKind;
use std::process::{Command, Stdio};

use crate::{Failure, Request};

pub fn verify(_: &Request) -> Result<(), Failure> {
    // fprintd が、登録してある指紋と照らし合わせる
    let status = Command::new("fprintd-verify")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err(Failure::Denied),
        Err(error) if error.kind() == ErrorKind::NotFound => Err(Failure::Unavailable),
        Err(error) => Err(Failure::Os(error.to_string())),
    }
}
