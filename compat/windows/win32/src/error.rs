// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-3.0-or-later
// Sources: Microsoft Learn "System Error Codes". Behaviour tests: the unit tests
//   below; wintest.exe checks the codes programs see.
//! Win32 error codes (the values `GetLastError` returns).

use omit_abi::Errno;

/// The operation succeeded.
pub const ERROR_SUCCESS: u32 = 0;
/// The system cannot find the file specified.
pub const ERROR_FILE_NOT_FOUND: u32 = 2;
/// The system cannot find the path specified.
pub const ERROR_PATH_NOT_FOUND: u32 = 3;
/// The system cannot open the file (too many open files).
pub const ERROR_TOO_MANY_OPEN_FILES: u32 = 4;
/// Access is denied.
pub const ERROR_ACCESS_DENIED: u32 = 5;
/// The handle is invalid.
pub const ERROR_INVALID_HANDLE: u32 = 6;
/// Not enough memory resources are available.
pub const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
/// The request is not supported.
pub const ERROR_NOT_SUPPORTED: u32 = 50;
/// The network path was not found.
pub const ERROR_BAD_NETPATH: u32 = 53;
/// The file exists.
pub const ERROR_FILE_EXISTS: u32 = 80;
/// The parameter is incorrect.
pub const ERROR_INVALID_PARAMETER: u32 = 87;
/// There is not enough space on the disk.
pub const ERROR_DISK_FULL: u32 = 112;
/// The data area passed to a system call is too small.
pub const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
/// The filename, directory name, or volume label syntax is incorrect.
pub const ERROR_INVALID_NAME: u32 = 123;
/// The specified module could not be found.
pub const ERROR_MOD_NOT_FOUND: u32 = 126;
/// The wait operation timed out.
pub const WAIT_TIMEOUT: u32 = 258;
/// The pipe has been ended.
pub const ERROR_BROKEN_PIPE: u32 = 109;
/// The specified procedure could not be found.
pub const ERROR_PROC_NOT_FOUND: u32 = 127;
/// A dynamic link library (DLL) initialization routine failed.
pub const ERROR_DLL_INIT_FAILED: u32 = 1114;
/// The directory is not empty.
pub const ERROR_DIR_NOT_EMPTY: u32 = 145;
/// Cannot create a file when that file already exists.
pub const ERROR_ALREADY_EXISTS: u32 = 183;
/// The system could not find the environment option that was entered.
pub const ERROR_ENVVAR_NOT_FOUND: u32 = 203;
/// The directory name is invalid.
pub const ERROR_DIRECTORY: u32 = 267;
/// Attempt to access invalid address.
pub const ERROR_INVALID_ADDRESS: u32 = 487;
/// The request could not be performed because of an I/O device error.
pub const ERROR_IO_DEVICE: u32 = 1117;
/// The system cannot move the file to a different disk drive.
pub const ERROR_NOT_SAME_DEVICE: u32 = 17;
/// The requested resource is in use.
pub const ERROR_BUSY: u32 = 170;
/// The file is not a valid Win32 application.
pub const ERROR_BAD_EXE_FORMAT: u32 = 193;
/// This function is not supported on this system.
pub const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;

/// The Win32 error code for a failed Omit OS system call.
pub fn from_errno(errno: Errno) -> u32 {
    match errno {
        Errno::NotFound => ERROR_FILE_NOT_FOUND,
        Errno::NotADirectory => ERROR_PATH_NOT_FOUND,
        Errno::BadFileDescriptor => ERROR_INVALID_HANDLE,
        Errno::Fault => ERROR_INVALID_ADDRESS,
        Errno::Exists => ERROR_ALREADY_EXISTS,
        Errno::IsADirectory => ERROR_ACCESS_DENIED,
        Errno::InvalidArgument => ERROR_INVALID_PARAMETER,
        Errno::TooManyOpenFiles => ERROR_TOO_MANY_OPEN_FILES,
        Errno::NoSpace => ERROR_DISK_FULL,
        Errno::NotEmpty => ERROR_DIR_NOT_EMPTY,
        Errno::NoSuchProcess => ERROR_INVALID_PARAMETER,
        Errno::Io => ERROR_IO_DEVICE,
        Errno::Busy => ERROR_BUSY,
        Errno::CrossDevice => ERROR_NOT_SAME_DEVICE,
        Errno::NotExecutable => ERROR_BAD_EXE_FORMAT,
        Errno::NoSuchSyscall => ERROR_CALL_NOT_IMPLEMENTED,
        Errno::BrokenPipe => ERROR_BROKEN_PIPE,
        Errno::WouldBlock => ERROR_BUSY,
        Errno::TimedOut => WAIT_TIMEOUT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_common_failures() {
        assert_eq!(from_errno(Errno::NotFound), 2);
        assert_eq!(from_errno(Errno::NotADirectory), 3);
        assert_eq!(from_errno(Errno::Exists), 183);
        assert_eq!(from_errno(Errno::NoSpace), 112);
        assert_eq!(from_errno(Errno::NotEmpty), 145);
        assert_eq!(from_errno(Errno::IsADirectory), 5);
    }
}
