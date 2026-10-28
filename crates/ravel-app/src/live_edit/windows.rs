// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The named-pipe transport (Windows).
//!
//! The pipe is `\\.\pipe\ravel-live-edit-<user SID>`. Its security descriptor
//! grants full access to the current user and SYSTEM only (the Unix socket's
//! 0600), and `PIPE_REJECT_REMOTE_CLIENTS` keeps it off the network.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_PIPE_CONNECTED, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use super::{JobSender, serve_connection};

/// One pipe instance, closed on drop.
struct Pipe(HANDLE);

// SAFETY: a pipe handle is usable from any thread; each instance has one owner.
unsafe impl Send for Pipe {}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut read = 0u32;
        let len = buf.len().min(u32::MAX as usize) as u32;
        // SAFETY: `buf` is valid for `len` bytes; `read` outlives the call.
        let ok = unsafe { ReadFile(self.0, buf.as_mut_ptr().cast(), len, &mut read, null_mut()) };
        if ok != 0 {
            Ok(read as usize)
        } else if unsafe { GetLastError() } == ERROR_BROKEN_PIPE {
            Ok(0)
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut written = 0u32;
        let len = buf.len().min(u32::MAX as usize) as u32;
        // SAFETY: `buf` is valid for `len` bytes; `written` outlives the call.
        let ok = unsafe { WriteFile(self.0, buf.as_ptr().cast(), len, &mut written, null_mut()) };
        if ok != 0 {
            Ok(written as usize)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: the handle is ours and closed exactly once.
        unsafe {
            // Disconnecting discards what the client has not read yet; wait
            // for it to take the last reply first.
            FlushFileBuffers(self.0);
            DisconnectNamedPipe(self.0);
            CloseHandle(self.0);
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Create one pipe instance reachable by the current user and SYSTEM only.
fn create_instance(name: &[u16], sid: &str, first: bool) -> io::Result<Pipe> {
    let sddl = wide(&format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)"));
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `sddl` is NUL-terminated; the descriptor is freed after the call
    // (`CreateNamedPipeW` copies it).
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let mut open_mode = PIPE_ACCESS_DUPLEX;
        if first {
            // Fails when an instance exists: another Ravel owns the pipe.
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        let handle = CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            &attributes,
        );
        let error = io::Error::last_os_error();
        LocalFree(descriptor);
        if handle == INVALID_HANDLE_VALUE {
            return Err(error);
        }
        Ok(Pipe(handle))
    }
}

/// Create the pipe and start accepting. `None` (logged) when it could not be
/// opened or another instance owns it. `endpoint` replaces the pipe name.
pub fn open(endpoint: Option<PathBuf>, tx: JobSender) -> Option<()> {
    let sid = match ravel_project::paths::current_user_sid() {
        Ok(sid) => sid,
        Err(error) => {
            tracing::warn!(%error, "could not read the user SID; the live-edit pipe stays closed");
            return None;
        }
    };
    let endpoint = endpoint.or_else(ravel_project::paths::live_edit_endpoint)?;
    let name = endpoint.to_string_lossy().into_owned();
    let name = wide(&name);
    let first = match create_instance(&name, &sid, true) {
        Ok(pipe) => pipe,
        Err(error) => {
            tracing::warn!(%error, "could not open the live-edit pipe (in use by another instance?)");
            return None;
        }
    };
    std::thread::Builder::new()
        .name("live-edit-accept".into())
        .spawn(move || accept_loop(first, name, sid, tx))
        .ok()?;
    Some(())
}

fn accept_loop(first: Pipe, name: Vec<u16>, sid: String, tx: JobSender) {
    let mut next = first;
    loop {
        // SAFETY: `next.0` is a live pipe handle; no OVERLAPPED is used.
        let connected = unsafe {
            ConnectNamedPipe(next.0, null_mut()) != 0 || GetLastError() == ERROR_PIPE_CONNECTED
        };
        let current = next;
        next = match create_instance(&name, &sid, false) {
            Ok(pipe) => pipe,
            Err(error) => {
                tracing::warn!(%error, "the live-edit pipe stopped accepting");
                return;
            }
        };
        if connected {
            let tx = tx.clone();
            let _ = std::thread::Builder::new()
                .name("live-edit-conn".into())
                .spawn(move || serve_connection(current, tx));
        }
    }
}
