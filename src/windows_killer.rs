//! Native Windows termination: portable-pty 0.9's cloned killer inverts the Win32 BOOL result.

use std::{
    io,
    os::windows::io::{AsRawHandle, BorrowedHandle, OwnedHandle},
    sync::Arc,
};

use portable_pty::{Child, ChildKiller};
use windows_sys::Win32::System::Threading::TerminateProcess;

#[derive(Clone, Debug)]
struct WindowsKiller(Arc<OwnedHandle>);

impl ChildKiller for WindowsKiller {
    fn kill(&mut self) -> io::Result<()> {
        // A nonzero BOOL means success; last-error is unspecified on success and can be stale.
        if unsafe { TerminateProcess(self.0.as_raw_handle().cast(), 1) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(self.clone())
    }
}

pub(crate) fn clone_killer(child: &dyn Child) -> io::Result<Box<dyn ChildKiller + Send + Sync>> {
    let raw = child
        .as_raw_handle()
        .ok_or_else(|| io::Error::other("the child has no Windows process handle"))?;
    // The child owns this handle for the borrow; the killer retains its own duplicate while wait runs.
    let handle = unsafe { BorrowedHandle::borrow_raw(raw) }.try_clone_to_owned()?;
    Ok(Box::new(WindowsKiller(Arc::new(handle))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::windows::{io::AsHandle, process::CommandExt},
        process::{Command, Stdio},
    };
    use windows_sys::Win32::{Foundation::SetLastError, System::Threading::CREATE_NO_WINDOW};

    #[test]
    fn successful_termination_ignores_a_stale_windows_error() {
        let mut child = Command::new("powershell")
            .args(["-NoProfile", "-Command", "[void][Console]::ReadLine()"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut killer = WindowsKiller(Arc::new(child.as_handle().try_clone_to_owned().unwrap()));
        unsafe { SetLastError(87) };
        let result = killer.kill();
        if result.is_err() {
            let _ = child.kill();
        }
        let status = child.wait().unwrap();
        assert!(
            result.is_ok(),
            "successful termination must not report {result:?}"
        );
        assert!(!status.success(), "the child really was terminated");
    }
}
