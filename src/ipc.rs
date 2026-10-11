//! Local IPC between `host` and `send`/`screen` (spec 004, spec 006).
//!
//! A Unix-domain socket on Unix, a named pipe on Windows. Both are reachable only by the user who
//! started the host.

use std::{io, path::Path};

use anyhow::Result;
use interprocess::local_socket::{
    GenericFilePath, ListenerNonblockingMode, ListenerOptions, Name, prelude::*,
};

pub use interprocess::local_socket::{Listener, Stream};

/// Starts listening on `socket`. Accepting does not block: `accept` returns `WouldBlock` when no
/// client is waiting. The Unix socket file is removed when the listener is dropped.
pub fn bind(socket: &Path) -> Result<Listener> {
    let options = ListenerOptions::new()
        .name(socket_name(socket)?)
        .nonblocking(ListenerNonblockingMode::Accept);
    #[cfg(windows)]
    let options = {
        use interprocess::os::windows::local_socket::ListenerOptionsExt;
        options.security_descriptor(windows::owner_only_descriptor()?)
    };
    let listener = options.create_sync().map_err(|error| {
        // Windows reports a pipe name that is already taken as "access denied".
        let hint = if cfg!(windows) && error.kind() == io::ErrorKind::PermissionDenied {
            " (already in use by another host?)"
        } else {
            ""
        };
        anyhow::Error::new(error)
            .context(format!("failed to bind socket {}{hint}", socket.display()))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(listener)
}

/// The next waiting client, or `WouldBlock` when there is none. The stream is always blocking:
/// BSD and macOS let an accepted socket inherit the listener's nonblocking mode.
pub fn accept(listener: &Listener) -> io::Result<Stream> {
    let stream = listener.accept()?;
    stream.set_nonblocking(false)?;
    Ok(stream)
}

pub fn connect(socket: &Path) -> io::Result<Stream> {
    Stream::connect(socket_name(socket)?)
}

#[cfg(unix)]
fn socket_name(socket: &Path) -> io::Result<Name<'static>> {
    socket.to_path_buf().to_fs_name::<GenericFilePath>()
}

/// On Windows `socket` is not a file: it is turned into the name of a pipe that is private to the
/// current user (see [`pipe_name`]). A value that already is a pipe path is used as given.
#[cfg(windows)]
fn socket_name(socket: &Path) -> io::Result<Name<'static>> {
    use interprocess::local_socket::GenericNamespaced;

    if is_pipe_path(socket) {
        return socket.to_path_buf().to_fs_name::<GenericFilePath>();
    }
    let absolute = std::path::absolute(socket)?;
    pipe_name(&absolute.to_string_lossy(), &windows::current_user_sid()?)
        .to_ns_name::<GenericNamespaced>()
}

/// `\\.\pipe\NAME` or `\\host\pipe\NAME`.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_pipe_path(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('/', "\\");
    text.strip_prefix(r"\\")
        .and_then(|rest| rest.split_once('\\'))
        .is_some_and(|(host, rest)| {
            !host.is_empty() && rest.to_ascii_lowercase().starts_with(r"pipe\") && rest.len() > 5
        })
}

/// Name (without the `\\.\pipe\` prefix) of the pipe that serves `path` for the user `sid`.
///
/// Windows paths are case-insensitive, so the path is lowercased. The user is part of the name so
/// that two users can use the same `--socket` value without colliding.
#[cfg_attr(not(windows), allow(dead_code))]
fn pipe_name(path: &str, sid: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in sid.bytes().chain([0]).chain(path.to_lowercase().bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("agent-cli-lab-{hash:016x}")
}

#[cfg(windows)]
mod windows {
    use std::{io, ptr};

    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    use widestring::U16CString;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, LocalFree},
        Security::{
            Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    /// The current user's SID in string form, for example `S-1-5-21-...-1001`.
    pub fn current_user_sid() -> io::Result<String> {
        struct Token(HANDLE);
        impl Drop for Token {
            fn drop(&mut self) {
                unsafe { CloseHandle(self.0) };
            }
        }

        unsafe {
            let mut handle: HANDLE = ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) == 0 {
                return Err(io::Error::last_os_error());
            }
            let token = Token(handle);

            let mut length = 0_u32;
            GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut length);
            if length == 0 {
                return Err(io::Error::last_os_error());
            }
            // u64 elements keep the buffer aligned for TOKEN_USER.
            let mut buffer = vec![0_u64; (length as usize).div_ceil(8)];
            if GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let user = &*buffer.as_ptr().cast::<TOKEN_USER>();

            let mut wide: *mut u16 = ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut wide) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut count = 0;
            while *wide.add(count) != 0 {
                count += 1;
            }
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(wide, count));
            LocalFree(wide.cast());
            Ok(sid)
        }
    }

    /// Protected DACL with one entry: full control for the current user. The default security of a
    /// named pipe also lets Everyone and Anonymous read from it.
    pub fn owner_only_descriptor() -> io::Result<SecurityDescriptor> {
        let sddl = format!("D:P(A;;GA;;;{})", current_user_sid()?);
        let sddl = U16CString::from_str(sddl).map_err(io::Error::other)?;
        SecurityDescriptor::deserialize(&sddl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_is_stable_and_case_insensitive() {
        let name = pipe_name(r"C:\Temp\Pi.sock", "S-1-5-21-1");
        assert_eq!(name, pipe_name(r"c:\temp\pi.sock", "S-1-5-21-1"));
        assert!(name.starts_with("agent-cli-lab-"));
        assert_eq!(name.len(), "agent-cli-lab-".len() + 16);
    }

    #[test]
    fn pipe_name_differs_by_path_and_user() {
        let name = pipe_name(r"C:\a", "S-1-5-21-1");
        assert_ne!(name, pipe_name(r"C:\b", "S-1-5-21-1"));
        assert_ne!(name, pipe_name(r"C:\a", "S-1-5-21-2"));
    }

    #[test]
    fn recognizes_pipe_paths() {
        assert!(is_pipe_path(Path::new(r"\\.\pipe\agent")));
        assert!(is_pipe_path(Path::new("//./pipe/agent")));
        assert!(is_pipe_path(Path::new(r"\\server\PIPE\agent")));
        assert!(!is_pipe_path(Path::new(r"\\.\pipe\")));
        assert!(!is_pipe_path(Path::new(r"\\server\share\pipe")));
        assert!(!is_pipe_path(Path::new("/tmp/pi.sock")));
        assert!(!is_pipe_path(Path::new(r"C:\Temp\pi.sock")));
    }
}

/// Access control of the Windows pipe (spec 006): only the current user can open it.
#[cfg(all(test, windows))]
mod access_tests {
    use std::{
        ptr,
        sync::atomic::{AtomicBool, Ordering},
        thread,
        time::Duration,
    };

    use widestring::U16CString;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_PIPE_BUSY, HANDLE, LocalFree},
        Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW,
                ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW,
                GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
            },
            CreateRestrictedToken, DACL_SECURITY_INFORMATION, DuplicateTokenEx,
            PSECURITY_DESCRIPTOR, SID_AND_ATTRIBUTES, SecurityImpersonation, TOKEN_DUPLICATE,
            TOKEN_IMPERSONATE, TOKEN_QUERY, TokenImpersonation,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken, SetThreadToken},
    };

    use super::*;

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Impersonation token of the current user, optionally with the user's own SID reduced to
    /// "deny only". Its access checks then behave like those of a different user: no allow entry
    /// for the current user applies, and the user still has the same group memberships.
    fn impersonation_token(user_sid: &str, as_other_user: bool) -> Handle {
        unsafe {
            let mut process_token: HANDLE = ptr::null_mut();
            assert_ne!(
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_IMPERSONATE,
                    &mut process_token
                ),
                0
            );
            let process_token = Handle(process_token);
            let mut token: HANDLE = ptr::null_mut();
            assert_ne!(
                DuplicateTokenEx(
                    process_token.0,
                    TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_IMPERSONATE,
                    ptr::null(),
                    SecurityImpersonation,
                    TokenImpersonation,
                    &mut token
                ),
                0
            );
            let token = Handle(token);
            if !as_other_user {
                return token;
            }
            let wide = U16CString::from_str(user_sid).unwrap();
            let mut sid = ptr::null_mut();
            assert_ne!(ConvertStringSidToSidW(wide.as_ptr(), &mut sid), 0);
            let disable = SID_AND_ATTRIBUTES {
                Sid: sid,
                Attributes: 0,
            };
            let mut restricted: HANDLE = ptr::null_mut();
            let created = CreateRestrictedToken(
                token.0,
                0,
                1,
                &disable,
                0,
                ptr::null(),
                0,
                ptr::null(),
                &mut restricted,
            );
            LocalFree(sid.cast());
            assert_ne!(created, 0, "{}", io::Error::last_os_error());
            Handle(restricted)
        }
    }

    /// Opens the pipe the way a client does (read and write) while impersonating `token`. The
    /// path must be worked out beforehand: looking up the user's SID needs the real identity.
    /// A pipe that is busy with another client is retried; any other error is returned.
    fn open_as(token: &Handle, pipe: &str) -> io::Result<()> {
        for _ in 0..200 {
            unsafe {
                assert_ne!(SetThreadToken(ptr::null(), token.0), 0);
            }
            let result = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(pipe);
            unsafe {
                SetThreadToken(ptr::null(), ptr::null_mut());
            }
            match result {
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    thread::sleep(Duration::from_millis(20));
                }
                other => return other.map(drop),
            }
        }
        Err(io::ErrorKind::TimedOut.into())
    }

    fn dacl_of(pipe: &str) -> String {
        let wide = U16CString::from_str(pipe).unwrap();
        for _ in 0..100 {
            unsafe {
                let mut descriptor = ptr::null_mut();
                let status = GetNamedSecurityInfoW(
                    wide.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut descriptor,
                );
                if status == 0 {
                    let sddl = dacl_text(descriptor);
                    LocalFree(descriptor);
                    return sddl;
                }
            }
            // The pipe was busy with another client; wait for a new instance.
            thread::sleep(Duration::from_millis(20));
        }
        panic!("could not read the pipe's security descriptor");
    }

    /// The DACL of `descriptor` as SDDL text, spelled the way Windows spells it.
    ///
    /// # Safety
    /// `descriptor` must point to a valid security descriptor.
    unsafe fn dacl_text(descriptor: PSECURITY_DESCRIPTOR) -> String {
        unsafe {
            let mut text: *mut u16 = ptr::null_mut();
            assert_ne!(
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    descriptor,
                    SDDL_REVISION_1,
                    DACL_SECURITY_INFORMATION,
                    &mut text,
                    ptr::null_mut()
                ),
                0
            );
            let mut count = 0;
            while *text.add(count) != 0 {
                count += 1;
            }
            let sddl = String::from_utf16_lossy(std::slice::from_raw_parts(text, count));
            LocalFree(text.cast());
            sddl
        }
    }

    /// Spells an SDDL string the way Windows does. One SID can be written in several ways: the
    /// built-in Administrator account (RID 500, the account of a CI runner) is read back as `LA`,
    /// not as its full SID. Strings are only comparable after this round trip.
    fn normalized(sddl: &str) -> String {
        let wide = U16CString::from_str(sddl).unwrap();
        unsafe {
            let mut descriptor = ptr::null_mut();
            assert_ne!(
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    ptr::null_mut()
                ),
                0,
                "{}",
                io::Error::last_os_error()
            );
            let text = dacl_text(descriptor);
            LocalFree(descriptor);
            text
        }
    }

    #[test]
    fn a_sid_and_its_alias_compare_equal_once_normalized() {
        let by_sid = "D:P(A;;FA;;;S-1-5-32-544)";
        let by_alias = "D:P(A;;FA;;;BA)";
        assert_ne!(by_sid, by_alias);
        assert_eq!(normalized(by_sid), normalized(by_alias));
        // Normalizing must not blur different principals or rights.
        assert_ne!(normalized(by_alias), normalized("D:P(A;;FA;;;WD)"));
        assert_ne!(normalized(by_alias), normalized("D:P(A;;FR;;;BA)"));
    }

    fn pipe_path(socket: &Path) -> String {
        let absolute = std::path::absolute(socket).unwrap();
        let name = pipe_name(
            &absolute.to_string_lossy(),
            &windows::current_user_sid().unwrap(),
        );
        format!(r"\\.\pipe\{name}")
    }

    #[test]
    fn only_the_current_user_can_open_the_pipe() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("acl.sock");
        let listener = bind(&socket).unwrap();
        let sid = windows::current_user_sid().unwrap();
        let pipe = pipe_path(&socket);
        let stop = AtomicBool::new(false);

        // Stops the accept thread even when an assertion below panics, so a failure cannot hang.
        struct StopOnDrop<'a>(&'a AtomicBool);
        impl Drop for StopOnDrop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        thread::scope(|scope| {
            let _stop = StopOnDrop(&stop);
            // Keep the nonblocking listener accepting, as `host` does.
            scope.spawn(|| {
                while !stop.load(Ordering::Acquire) {
                    if accept(&listener).is_err() {
                        thread::sleep(Duration::from_millis(5));
                    }
                }
            });

            assert_eq!(
                dacl_of(&pipe),
                normalized(&format!("D:P(A;;FA;;;{sid})")),
                "exactly one protected entry, for the current user"
            );
            open_as(&impersonation_token(&sid, false), &pipe)
                .expect("the current user can open the pipe");
            let denied = open_as(&impersonation_token(&sid, true), &pipe)
                .expect_err("another identity must not open the pipe");
            assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied, "{denied}");
        });
    }

    #[test]
    fn a_second_listener_on_the_same_name_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("twice.sock");
        let _first = bind(&socket).unwrap();
        let error = bind(&socket).expect_err("the name is taken");
        assert!(format!("{error:#}").contains("already in use"), "{error:#}");
    }
}
