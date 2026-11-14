//! Windows socket readiness through the Ancillary Function Driver (AFD).
//!
//! Winsock sockets are AFD file objects. Instead of issuing overlapped
//! `ReadFile`/`WriteFile` on them, the IOCP driver opens one private AFD
//! device handle (`\Device\Afd\VesperPulse`), associates it with its
//! completion port and issues `IOCTL_AFD_POLL` on it for each pending socket
//! request. The poll's packet reports which readiness events fired; the
//! driver then performs a non-blocking `recv`/`send` and either completes the
//! request or re-arms the poll on `WSAEWOULDBLOCK`. This is the mechanism
//! behind wepoll, mio and libuv on Windows; it never lends a buffer to the
//! kernel and never binds the socket itself to a completion port, so a
//! socket can be submitted to any driver and closed at any time.
#![allow(unsafe_code)]
use crate::driver::Operation;
use std::{
    fs::File,
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
};
use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{FILE_OPEN, NtCreateFile};
use windows_sys::Win32::Foundation::{
    ERROR_IO_PENDING, GetLastError, HANDLE, RtlNtStatusToDosError, STATUS_CANCELLED, UNICODE_STRING,
};
use windows_sys::Win32::Networking::WinSock::{
    FIONBIO, SIO_BASE_HANDLE, SO_TYPE, SOCKET, SOCKET_ERROR, SOL_SOCKET, WSAEWOULDBLOCK,
    WSAGetLastError, WSAIoctl, getsockopt, ioctlsocket, recv, send,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_PIPE, GetFileType, SYNCHRONIZE,
};
use windows_sys::Win32::System::IO::{DeviceIoControl, IO_STATUS_BLOCK, OVERLAPPED};
use windows_sys::Win32::System::Pipes::GetNamedPipeInfo;

const IOCTL_AFD_POLL: u32 = 0x0001_2024;

pub(crate) const AFD_POLL_RECEIVE: u32 = 0x0001;
pub(crate) const AFD_POLL_SEND: u32 = 0x0004;
pub(crate) const AFD_POLL_DISCONNECT: u32 = 0x0008;
pub(crate) const AFD_POLL_ABORT: u32 = 0x0010;
pub(crate) const AFD_POLL_LOCAL_CLOSE: u32 = 0x0020;
pub(crate) const AFD_POLL_CONNECT_FAIL: u32 = 0x0100;

/// `AFD_POLL_HANDLE_INFO`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct PollHandle {
    pub handle: HANDLE,
    pub events: u32,
    pub status: i32,
}

/// `AFD_POLL_INFO` with one handle. It is both the input and the output of
/// the poll, so it lives pinned beside the request's `OVERLAPPED`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct PollInfo {
    pub timeout: i64,
    pub number_of_handles: u32,
    pub exclusive: u32,
    pub handles: [PollHandle; 1],
}

impl PollInfo {
    pub(crate) fn zeroed() -> Self {
        PollInfo {
            timeout: 0,
            number_of_handles: 0,
            exclusive: 0,
            handles: [PollHandle {
                handle: std::ptr::null_mut(),
                events: 0,
                status: 0,
            }],
        }
    }
}

/// The events that make an operation worth attempting: its own readiness,
/// or any condition under which it fails at once.
pub(crate) fn interest(op: Operation) -> u32 {
    let failure = AFD_POLL_ABORT | AFD_POLL_LOCAL_CLOSE | AFD_POLL_CONNECT_FAIL;
    match op {
        Operation::Read => AFD_POLL_RECEIVE | AFD_POLL_DISCONNECT | failure,
        Operation::Write => AFD_POLL_SEND | failure,
    }
}

/// A socket behind `file`: `(socket, base provider handle)`. `None` for any
/// handle that is not a Winsock socket (or when Winsock is not loaded, in
/// which case no socket can exist).
pub(crate) fn socket_of(file: &File) -> Option<(SOCKET, SOCKET)> {
    let h = file.as_raw_handle() as HANDLE;
    // Sockets report FILE_TYPE_PIPE but are not named pipes. Checking this
    // first keeps Winsock calls off files and pipes (some Winsock layers,
    // Wine's among them, answer SIO_BASE_HANDLE for any handle).
    // SAFETY: both calls only inspect a live handle; null out-pointers are
    // permitted by GetNamedPipeInfo.
    let pipe_like = unsafe { GetFileType(h) } == FILE_TYPE_PIPE;
    if !pipe_like
        || unsafe {
            GetNamedPipeInfo(
                h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } != 0
    {
        return None;
    }
    let socket = h as SOCKET;
    let mut kind = 0i32;
    let mut len = std::mem::size_of::<i32>() as i32;
    // SAFETY: the option buffer is a live i32 and `len` its size.
    if unsafe {
        getsockopt(
            socket,
            SOL_SOCKET,
            SO_TYPE,
            (&mut kind as *mut i32).cast(),
            &mut len,
        )
    } == SOCKET_ERROR
    {
        return None;
    }
    let mut base: SOCKET = 0;
    let mut bytes = 0u32;
    // SAFETY: the output buffer is a live SOCKET-sized local; no overlapped.
    let rc = unsafe {
        WSAIoctl(
            socket,
            SIO_BASE_HANDLE,
            std::ptr::null(),
            0,
            (&mut base as *mut SOCKET).cast(),
            std::mem::size_of::<SOCKET>() as u32,
            &mut bytes,
            std::ptr::null_mut(),
            None,
        )
    };
    (rc != SOCKET_ERROR && base != 0).then_some((socket, base))
}

/// Put the socket in non-blocking mode (left set, like the readiness
/// drivers' `O_NONBLOCK`).
pub(crate) fn set_nonblocking(socket: SOCKET) -> io::Result<()> {
    let mut on = 1u32;
    // SAFETY: FIONBIO reads one u32 from a live local.
    if unsafe { ioctlsocket(socket, FIONBIO, &mut on) } == SOCKET_ERROR {
        return Err(wsa_error());
    }
    Ok(())
}

fn wsa_error() -> io::Error {
    // SAFETY: reads the calling thread's Winsock error.
    io::Error::from_raw_os_error(unsafe { WSAGetLastError() })
}

/// One non-blocking transfer. `Err(WouldBlock)` means "not ready yet".
pub(crate) fn transfer(socket: SOCKET, op: Operation, buffer: &mut [u8]) -> io::Result<usize> {
    let len = buffer.len().min(i32::MAX as usize) as i32;
    // SAFETY: the buffer is live and at least `len` bytes long.
    let n = unsafe {
        match op {
            Operation::Read => recv(socket, buffer.as_mut_ptr(), len, 0),
            Operation::Write => send(socket, buffer.as_ptr(), len, 0),
        }
    };
    if n == SOCKET_ERROR {
        let e = wsa_error();
        if e.raw_os_error() == Some(WSAEWOULDBLOCK) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        return Err(e);
    }
    Ok(n as usize)
}

/// Open a private AFD device handle for polling.
pub(crate) fn open() -> io::Result<OwnedHandle> {
    let name: Vec<u16> = r"\Device\Afd\VesperPulse".encode_utf16().collect();
    let mut unicode = UNICODE_STRING {
        Length: (name.len() * 2) as u16,
        MaximumLength: (name.len() * 2) as u16,
        Buffer: name.as_ptr() as *mut u16,
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: std::ptr::null_mut(),
        ObjectName: &mut unicode,
        Attributes: 0,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: plain data; zero is a valid initial status block.
    let mut status: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer refers to a live local for the duration.
    let rc = unsafe {
        NtCreateFile(
            &mut handle,
            SYNCHRONIZE,
            &attributes,
            &mut status,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_OPEN,
            0,
            std::ptr::null(),
            0,
        )
    };
    if rc < 0 {
        return Err(ntstatus_error(rc));
    }
    // SAFETY: a fresh handle owned from here on.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) })
}

pub(crate) fn ntstatus_error(status: i32) -> io::Error {
    if status == STATUS_CANCELLED {
        return crate::driver::cancelled_error();
    }
    // SAFETY: pure conversion.
    let code = unsafe { RtlNtStatusToDosError(status) };
    io::Error::from_raw_os_error(code as i32)
}

/// Issue `IOCTL_AFD_POLL` for `base` on the AFD handle. The packet arrives
/// on the port the AFD handle is associated with, carrying `ov`.
///
/// # Safety
/// `ov` and `info` must stay valid and unmoved until that packet has been
/// dequeued.
pub(crate) unsafe fn arm(
    afd: HANDLE,
    ov: *mut OVERLAPPED,
    info: *mut PollInfo,
    base: SOCKET,
    events: u32,
) -> io::Result<()> {
    // SAFETY: the caller keeps both pointers valid; writing through them is
    // exclusive because the driver owns the slot.
    unsafe {
        *ov = std::mem::zeroed();
        *info = PollInfo {
            timeout: i64::MAX,
            number_of_handles: 1,
            exclusive: 0,
            handles: [PollHandle {
                handle: base as HANDLE,
                events,
                status: 0,
            }],
        };
    }
    let size = std::mem::size_of::<PollInfo>() as u32;
    // SAFETY: as above; the AFD handle is live and overlapped.
    let ok = unsafe {
        DeviceIoControl(
            afd,
            IOCTL_AFD_POLL,
            info as *const _,
            size,
            info.cast(),
            size,
            std::ptr::null_mut(),
            ov,
        )
    } != 0;
    // SAFETY: reads the calling thread's last error.
    if ok || unsafe { GetLastError() } == ERROR_IO_PENDING {
        // A poll that finished at once still queues its packet: the AFD
        // handle does not skip the port on success.
        return Ok(());
    }
    Err(io::Error::last_os_error())
}
