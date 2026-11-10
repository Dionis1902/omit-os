// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Exercises the Windows API functions the compatibility layer provides
//! and checks each result, including the documented error codes. Prints
//! one line per group of checks and returns exit code 42 from the entry
//! point (rather than calling `ExitProcess`) when everything passed.

#![no_std]
#![no_main]

use core::arch::asm;
use core::ffi::CStr;
use core::hint::black_box;
use core::ptr::{null, null_mut};

use omit_win_samples::*;

fn double(value: u32) -> u32 {
    value * 2
}
fn square(value: u32) -> u32 {
    value * value
}

/// Data holding absolute addresses: pointers to strings and functions.
/// These only work if the loader applied the image's base relocations.
static WORDS: [&str; 3] = ["relocated", "data", "works"];
static FUNCTIONS: [fn(u32) -> u32; 2] = [double, square];

fn relocations(checks: &mut Checks) {
    let words = black_box(&WORDS);
    let functions = black_box(&FUNCTIONS);
    checks.check(
        functions[0](21) == 42 && functions[1](7) == 49,
        "function pointers",
    );
    say!("wintest: {} {} {}", words[0], words[1], words[2]);
}

fn environment_block(checks: &mut Checks) {
    let (teb, peb): (usize, usize);
    // SAFETY: on 64-bit Windows GS addresses the thread environment
    // block, which holds its own address at 0x30 and the process
    // environment block's at 0x60.
    unsafe {
        asm!(
            "mov {teb}, gs:[0x30]",
            "mov {peb}, gs:[0x60]",
            teb = out(reg) teb,
            peb = out(reg) peb,
            options(nostack, readonly, preserves_flags),
        );
    }
    checks.check(teb != 0 && peb != 0, "TEB and PEB present");
    // SAFETY: the PEB holds the image base at 0x10; null asks for the
    // program's own module.
    let (from_peb, module) = unsafe {
        (
            *((peb + 0x10) as *const usize),
            GetModuleHandleA(null_mut()) as usize,
        )
    };
    checks.check(from_peb == module && module != 0, "image base in PEB");
    // SAFETY: the module handle is the address of the image, which starts
    // with the DOS header.
    let signature = unsafe { *(module as *const [u8; 2]) };
    checks.check(&signature == b"MZ", "module handle points at the image");
    checks.check(GetCurrentProcessId() != 0, "process id");
    say!(
        "wintest: image loaded at a relocated base: {}",
        module != 0x1_4000_0000
    );
}

fn heap(checks: &mut Checks) {
    let heap = GetProcessHeap();
    let block = HeapAlloc(heap, HEAP_ZERO_MEMORY, 100);
    checks.check(
        !block.is_null() && (block as usize).is_multiple_of(16),
        "HeapAlloc",
    );
    // SAFETY: `block` is a live 100-byte heap block; it is reallocated
    // and freed exactly once, and only accessed within its size.
    unsafe {
        let bytes = core::slice::from_raw_parts_mut(block, 100);
        checks.check(bytes.iter().all(|&b| b == 0), "HEAP_ZERO_MEMORY");
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = index as u8;
        }
        checks.check(HeapSize(heap, 0, block) == 100, "HeapSize");
        let bigger = HeapReAlloc(heap, HEAP_ZERO_MEMORY, block, 50_000);
        checks.check(!bigger.is_null(), "HeapReAlloc");
        let bytes = core::slice::from_raw_parts(bigger, 50_000);
        checks.check(
            bytes[..100].iter().enumerate().all(|(i, &b)| b == i as u8),
            "HeapReAlloc keeps contents",
        );
        checks.check(
            bytes[100..].iter().all(|&b| b == 0),
            "HeapReAlloc zeroes growth",
        );
        checks.check(HeapSize(heap, 0, bigger) == 50_000, "HeapSize after growth");
        checks.check(HeapFree(heap, 0, bigger) != 0, "HeapFree");
        checks.check(HeapFree(heap, 0, null_mut()) != 0, "HeapFree(NULL)");
    }

    let size = 3 * 64 * 1024;
    let region = VirtualAlloc(null_mut(), size, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    checks.check(
        !region.is_null() && (region as usize).is_multiple_of(0x1_0000),
        "VirtualAlloc",
    );
    // SAFETY: `region` is `size` bytes of fresh read/write memory,
    // released once below and not used afterwards.
    unsafe {
        let bytes = core::slice::from_raw_parts_mut(region.cast::<u8>(), size);
        checks.check(bytes.iter().all(|&b| b == 0), "VirtualAlloc zeroes");
        bytes[size - 1] = 0xA5;
        checks.check(
            black_box(bytes[size - 1]) == 0xA5,
            "VirtualAlloc memory usable",
        );
        SetLastError(0);
        checks.check(
            VirtualFree(region, 1, MEM_RELEASE) == 0,
            "VirtualFree with a size",
        );
        checks.check(GetLastError() == 87, "ERROR_INVALID_PARAMETER");
        checks.check(VirtualFree(region, 0, MEM_RELEASE) != 0, "VirtualFree");
        checks.check(
            VirtualFree(region, 0, MEM_RELEASE) == 0,
            "double VirtualFree",
        );
        checks.check(GetLastError() == 487, "ERROR_INVALID_ADDRESS");
    }
    // Writable and executable at once is refused by design.
    let refused = VirtualAlloc(
        null_mut(),
        4096,
        MEM_COMMIT | MEM_RESERVE,
        PAGE_EXECUTE_READWRITE,
    );
    checks.check(
        refused.is_null() && GetLastError() == 50,
        "W+X memory refused",
    );
    say!("wintest: heap and virtual memory ok");
}

fn attributes(path: &CStr) -> u32 {
    // SAFETY: `path` is NUL-terminated.
    unsafe { GetFileAttributesA(path.as_ptr()) }
}

fn files(checks: &mut Checks) {
    let directory = c"C:\\wintest.dir";
    let first = c"C:\\wintest.dir\\first.txt";
    let second = c"C:\\wintest.dir\\second.txt";
    let text = b"written through WriteFile\r\n";

    // SAFETY: every pointer passed below is a NUL-terminated string or a
    // live buffer of the stated length; handles are used while open.
    unsafe {
        checks.check(
            CreateDirectoryA(directory.as_ptr(), null_mut()) != 0,
            "CreateDirectoryA",
        );
        checks.check(
            CreateDirectoryA(directory.as_ptr(), null_mut()) == 0,
            "directory exists",
        );
        checks.check(GetLastError() == 183, "ERROR_ALREADY_EXISTS");
        checks.check(
            attributes(directory) == FILE_ATTRIBUTE_DIRECTORY,
            "directory attributes",
        );

        let file = CreateFileA(
            first.as_ptr(),
            GENERIC_WRITE,
            0,
            null_mut(),
            CREATE_NEW,
            0,
            null_mut(),
        );
        checks.check(file != INVALID_HANDLE_VALUE, "CreateFileA(CREATE_NEW)");
        let mut count = 0;
        checks.check(
            WriteFile(
                file,
                text.as_ptr(),
                text.len() as u32,
                &raw mut count,
                null_mut(),
            ) != 0
                && count as usize == text.len(),
            "WriteFile to a file",
        );
        let mut byte = 0u8;
        checks.check(
            ReadFile(file, &raw mut byte, 1, &raw mut count, null_mut()) == 0
                && GetLastError() == 5,
            "reading a write-only handle is denied",
        );
        checks.check(CloseHandle(file) != 0, "CloseHandle");
        checks.check(
            CloseHandle(file) == 0 && GetLastError() == 6,
            "ERROR_INVALID_HANDLE",
        );

        let again = CreateFileA(
            first.as_ptr(),
            GENERIC_WRITE,
            0,
            null_mut(),
            CREATE_NEW,
            0,
            null_mut(),
        );
        checks.check(
            again == INVALID_HANDLE_VALUE && GetLastError() == 80,
            "ERROR_FILE_EXISTS",
        );
        let both = CreateFileA(
            first.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            null_mut(),
            OPEN_EXISTING,
            0,
            null_mut(),
        );
        // Read and write through one handle, moving the position:
        // overwrite the middle, read the whole back, extend the file.
        checks.check(both != INVALID_HANDLE_VALUE, "open for reading and writing");
        let mut position = -1i64;
        let mut buffer = [0u8; 64];
        let size_before = {
            let mut size = 0i64;
            GetFileSizeEx(both, &raw mut size);
            size
        };
        checks.check(
            SetFilePointerEx(both, 0, &raw mut position, FILE_END) != 0
                && position == size_before
                && size_before >= 4,
            "SetFilePointerEx to the end",
        );
        checks.check(
            SetFilePointerEx(both, 1, null_mut(), FILE_BEGIN) != 0
                && WriteFile(both, b"XY".as_ptr(), 2, &raw mut count, null_mut()) != 0
                && count == 2,
            "overwrite in the middle",
        );
        checks.check(
            SetFilePointer(both, 0, null_mut(), FILE_CURRENT) == 3,
            "SetFilePointer reports the position",
        );
        checks.check(
            SetFilePointerEx(both, -3, &raw mut position, FILE_CURRENT) != 0 && position == 0,
            "seek back relative to the position",
        );
        checks.check(
            ReadFile(both, buffer.as_mut_ptr(), 64, &raw mut count, null_mut()) != 0
                && i64::from(count) == size_before
                && &buffer[1..3] == b"XY",
            "read back through the same handle",
        );
        checks.check(
            SetFilePointerEx(both, -1, null_mut(), FILE_BEGIN) == 0
                && GetLastError() == ERROR_NEGATIVE_SEEK,
            "ERROR_NEGATIVE_SEEK",
        );
        checks.check(
            SetFilePointerEx(both, 0, null_mut(), 9) == 0,
            "an unknown seek method is refused",
        );
        // Put the two bytes back as they were, so that the checks further
        // down see the file they expect.
        SetFilePointerEx(both, 1, null_mut(), FILE_BEGIN);
        checks.check(
            WriteFile(both, text[1..3].as_ptr(), 2, &raw mut count, null_mut()) != 0,
            "restore the overwritten bytes",
        );
        CloseHandle(both);

        // Listing the directory, with a second file in it.
        let notes = c"C:\\wintest.dir\\notes.md";
        let created = CreateFileA(
            notes.as_ptr(),
            GENERIC_WRITE,
            0,
            null_mut(),
            CREATE_NEW,
            0,
            null_mut(),
        );
        CloseHandle(created);
        let mut found = FindData::new();
        let listing = FindFirstFileA(c"C:\\wintest.dir\\*".as_ptr(), &raw mut found);
        checks.check(
            listing != INVALID_HANDLE_VALUE
                && found.name() == b"first.txt"
                && found.attributes == FILE_ATTRIBUTE_NORMAL
                && found.size_low as usize == text.len()
                && found.size_high == 0,
            "FindFirstFileA reports the first file and its size",
        );
        checks.check(
            FindNextFileA(listing, &raw mut found) != 0
                && found.name() == b"notes.md"
                && found.size_low == 0,
            "FindNextFileA reports the next",
        );
        checks.check(
            FindNextFileA(listing, &raw mut found) == 0 && GetLastError() == ERROR_NO_MORE_FILES,
            "ERROR_NO_MORE_FILES",
        );
        checks.check(FindClose(listing) != 0, "FindClose");
        checks.check(FindClose(listing) == 0, "a listing is closed once");
        // Patterns: case does not matter; a directory is found by name.
        let listing = FindFirstFileA(c"C:\\wintest.dir\\*.MD".as_ptr(), &raw mut found);
        checks.check(
            listing != INVALID_HANDLE_VALUE
                && found.name() == b"notes.md"
                && FindNextFileA(listing, &raw mut found) == 0,
            "a pattern selects by extension",
        );
        FindClose(listing);
        let listing = FindFirstFileA(c"C:\\wintest.d?r".as_ptr(), &raw mut found);
        checks.check(
            listing != INVALID_HANDLE_VALUE
                && found.name() == b"wintest.dir"
                && found.attributes == FILE_ATTRIBUTE_DIRECTORY,
            "a directory is listed as one",
        );
        FindClose(listing);
        checks.check(
            FindFirstFileA(c"C:\\wintest.dir\\*.zip".as_ptr(), &raw mut found)
                == INVALID_HANDLE_VALUE
                && GetLastError() == ERROR_FILE_NOT_FOUND,
            "no match is ERROR_FILE_NOT_FOUND",
        );
        checks.check(
            FindFirstFileA(c"C:\\nosuch.dir\\*".as_ptr(), &raw mut found) == INVALID_HANDLE_VALUE
                && GetLastError() == ERROR_PATH_NOT_FOUND,
            "a missing directory is ERROR_PATH_NOT_FOUND",
        );
        checks.check(DeleteFileA(notes.as_ptr()) != 0, "remove the second file");

        checks.check(MoveFileA(first.as_ptr(), second.as_ptr()) != 0, "MoveFileA");
        checks.check(
            attributes(first) == INVALID_FILE_ATTRIBUTES,
            "old name is gone",
        );
        checks.check(GetLastError() == 2, "ERROR_FILE_NOT_FOUND");
        checks.check(attributes(second) == 0x80, "FILE_ATTRIBUTE_NORMAL");

        let file = CreateFileA(
            second.as_ptr(),
            GENERIC_READ,
            1,
            null_mut(),
            OPEN_EXISTING,
            0,
            null_mut(),
        );
        checks.check(file != INVALID_HANDLE_VALUE, "CreateFileA(OPEN_EXISTING)");
        let mut size = 0i64;
        checks.check(
            GetFileSizeEx(file, &raw mut size) != 0 && size as usize == text.len(),
            "GetFileSizeEx",
        );
        let mut buffer = [0u8; 64];
        checks.check(
            ReadFile(file, buffer.as_mut_ptr(), 64, &raw mut count, null_mut()) != 0
                && &buffer[..count as usize] == text,
            "ReadFile returns what was written",
        );
        checks.check(
            ReadFile(file, buffer.as_mut_ptr(), 64, &raw mut count, null_mut()) != 0 && count == 0,
            "ReadFile at end of file",
        );
        CloseHandle(file);

        // CREATE_ALWAYS empties an existing file and says it existed.
        let file = CreateFileA(
            second.as_ptr(),
            GENERIC_WRITE,
            0,
            null_mut(),
            CREATE_ALWAYS,
            0,
            null_mut(),
        );
        checks.check(
            file != INVALID_HANDLE_VALUE && GetLastError() == 183,
            "CREATE_ALWAYS",
        );
        checks.check(
            GetFileSizeEx(file, &raw mut size) != 0 && size == 0,
            "file was emptied",
        );
        CloseHandle(file);

        checks.check(
            RemoveDirectoryA(directory.as_ptr()) == 0,
            "directory not empty",
        );
        checks.check(GetLastError() == 145, "ERROR_DIR_NOT_EMPTY");
        checks.check(
            DeleteFileA(directory.as_ptr()) == 0 && GetLastError() == 5,
            "DeleteFileA on a directory",
        );
        checks.check(DeleteFileA(second.as_ptr()) != 0, "DeleteFileA");
        checks.check(
            DeleteFileA(second.as_ptr()) == 0 && GetLastError() == 2,
            "file is gone",
        );
        checks.check(
            RemoveDirectoryA(directory.as_ptr()) != 0,
            "RemoveDirectoryA",
        );
        checks.check(
            attributes(directory) == INVALID_FILE_ATTRIBUTES,
            "directory is gone",
        );

        // Paths that cannot exist here.
        let other_drive = CreateFileA(
            c"D:\\x".as_ptr(),
            GENERIC_READ,
            0,
            null_mut(),
            OPEN_EXISTING,
            0,
            null_mut(),
        );
        checks.check(
            other_drive == INVALID_HANDLE_VALUE && GetLastError() == 3,
            "ERROR_PATH_NOT_FOUND",
        );
        // The null device swallows everything.
        let null = CreateFileA(
            c"NUL".as_ptr(),
            GENERIC_WRITE,
            0,
            null_mut(),
            OPEN_EXISTING,
            0,
            null_mut(),
        );
        checks.check(
            WriteFile(
                null,
                text.as_ptr(),
                text.len() as u32,
                &raw mut count,
                null_mut(),
            ) != 0,
            "NUL device",
        );
        CloseHandle(null);
    }
    say!("wintest: files and directories ok");
}

fn console(checks: &mut Checks) {
    let wide: [u16; 18] = {
        let mut units = [0u16; 18];
        let text = b"wide characters ok";
        let mut index = 0;
        while index < text.len() {
            units[index] = text[index] as u16;
            index += 1;
        }
        units
    };
    let mut count = 0;
    let out = GetStdHandle(STD_OUTPUT_HANDLE);
    // SAFETY: live buffers of the stated lengths.
    unsafe {
        let _ = WriteFile(out, b"wintest: ".as_ptr(), 9, &raw mut count, null_mut());
        checks.check(
            WriteConsoleW(out, wide.as_ptr(), 18, &raw mut count, null_mut()) != 0 && count == 18,
            "WriteConsoleW",
        );
        let _ = WriteFile(out, b"\r\n".as_ptr(), 2, &raw mut count, null_mut());
        let error_text = b"wintest: this line goes to standard error\r\n";
        checks.check(
            WriteFile(
                GetStdHandle(STD_ERROR_HANDLE),
                error_text.as_ptr(),
                error_text.len() as u32,
                &raw mut count,
                null_mut(),
            ) != 0,
            "standard error",
        );
    }
    checks.check(
        GetStdHandle(7) == INVALID_HANDLE_VALUE,
        "bad standard handle",
    );
}

/// Starting other programs and waiting for them.
fn processes(checks: &mut Checks) {
    let mut information = core::mem::MaybeUninit::<ProcessInformation>::uninit();
    let mut code = 0u32;
    // SAFETY: the strings are NUL-terminated (command lines in writable
    // buffers, as the function requires); `information` is filled on
    // success before it is read.
    unsafe {
        // This program again, in a mode where it says a line and exits
        // with code 7. Its output goes where this program's goes.
        let mut command = *b"wintest.exe child\0";
        checks.check(
            CreateProcessA(
                null(),
                command.as_mut_ptr().cast(),
                null_mut(),
                null_mut(),
                0,
                0,
                null_mut(),
                null(),
                null_mut(),
                information.as_mut_ptr(),
            ) != 0,
            "CreateProcessA from a command line",
        );
        let child = information.assume_init_read();
        checks.check(
            WaitForSingleObject(child.process, 10_000) == WAIT_OBJECT_0,
            "wait for the program to end",
        );
        checks.check(
            GetExitCodeProcess(child.process, &raw mut code) != 0 && code == 7,
            "its exit code",
        );
        checks.check(
            WaitForSingleObject(child.thread, 0) == WAIT_OBJECT_0 && child.process_id != 0,
            "an ended program stays signalled",
        );
        checks.check(
            CloseHandle(child.process) != 0 && CloseHandle(child.thread) != 0,
            "close the process handles",
        );

        // Named by its file, with the command line left out.
        checks.check(
            CreateProcessA(
                c"C:\\bin\\winhello.exe".as_ptr(),
                null_mut(),
                null_mut(),
                null_mut(),
                0,
                0,
                null_mut(),
                null(),
                null_mut(),
                information.as_mut_ptr(),
            ) != 0,
            "CreateProcessA from a file name",
        );
        let child = information.assume_init_read();
        checks.check(
            WaitForSingleObject(child.process, INFINITE) == WAIT_OBJECT_0
                && GetExitCodeProcess(child.process, &raw mut code) != 0
                && code == 0,
            "wait without a time limit",
        );
        CloseHandle(child.process);
        CloseHandle(child.thread);

        // A native program that never ends by itself: still running
        // after a wait, then terminated.
        let mut command = *b"spin\0";
        checks.check(
            CreateProcessA(
                null(),
                command.as_mut_ptr().cast(),
                null_mut(),
                null_mut(),
                0,
                0,
                null_mut(),
                null(),
                null_mut(),
                information.as_mut_ptr(),
            ) != 0,
            "start a native program",
        );
        let child = information.assume_init_read();
        checks.check(
            WaitForSingleObject(child.process, 50) == WAIT_TIMEOUT,
            "a running program is not signalled",
        );
        checks.check(
            GetExitCodeProcess(child.process, &raw mut code) != 0 && code == STILL_ACTIVE,
            "STILL_ACTIVE",
        );
        checks.check(TerminateProcess(child.process, 1) != 0, "TerminateProcess");
        checks.check(
            WaitForSingleObject(child.process, 10_000) == WAIT_OBJECT_0
                && GetExitCodeProcess(child.process, &raw mut code) != 0
                && code != STILL_ACTIVE,
            "a terminated program has ended",
        );
        checks.check(
            TerminateProcess(child.process, 1) == 0,
            "an ended program cannot be terminated again",
        );
        CloseHandle(child.process);
        CloseHandle(child.thread);

        let mut command = *b"nosuch.exe\0";
        checks.check(
            CreateProcessA(
                null(),
                command.as_mut_ptr().cast(),
                null_mut(),
                null_mut(),
                0,
                0,
                null_mut(),
                null(),
                null_mut(),
                information.as_mut_ptr(),
            ) == 0
                && GetLastError() == ERROR_FILE_NOT_FOUND,
            "a missing program is ERROR_FILE_NOT_FOUND",
        );
        checks.check(
            GetExitCodeProcess(GetStdHandle(STD_OUTPUT_HANDLE), &raw mut code) == 0,
            "not every handle is a process",
        );
    }
    say!("wintest: processes ok");
}

fn time(checks: &mut Checks) {
    let before = GetTickCount64();
    Sleep(50);
    let elapsed = GetTickCount64() - before;
    checks.check((40..2000).contains(&elapsed), "Sleep(50) takes about 50 ms");

    // The date and time. The test machine's clock is set to a moment in
    // November 2026 (and the real date is not earlier than that).
    let (mut file_time, mut later) = (FileTime::default(), FileTime::default());
    let (mut system, mut local) = (SystemTime::default(), SystemTime::default());
    // SAFETY: the pointers refer to live structures of the right types.
    unsafe {
        GetSystemTimeAsFileTime(&raw mut file_time);
        GetSystemTime(&raw mut system);
        GetLocalTime(&raw mut local);
        Sleep(20);
        GetSystemTimeAsFileTime(&raw mut later);
    }
    // 2026-11-10 00:00:00 UTC as a file time, and one year in its units.
    const NOVEMBER_2026: u64 = 134_387_424_000_000_000;
    const YEAR: u64 = 365 * 86_400 * 10_000_000;
    checks.check(
        (NOVEMBER_2026..NOVEMBER_2026 + 100 * YEAR).contains(&file_time.value()),
        "the file time is a present-day date",
    );
    let advanced = later.value() - file_time.value();
    checks.check(
        (100_000..20_000_000).contains(&advanced),
        "the file time advances with the clock",
    );
    checks.check(
        system.year >= 2026
            && (1..=12).contains(&system.month)
            && (1..=31).contains(&system.day)
            && system.day_of_week < 7
            && system.hour < 24
            && system.minute < 60
            && system.second < 60
            && system.milliseconds < 1000,
        "the system time is a valid date",
    );
    checks.check(
        (local.year, local.month, local.day) == (system.year, system.month, system.day),
        "local time is UTC",
    );
    say!(
        "wintest: date {}-{:02}-{:02}, day {} of the week",
        system.year,
        system.month,
        system.day,
        system.day_of_week
    );
    say!("wintest: time ok");
}

/// Entry point.
#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> u32 {
    if command_line().ends_with(" child") {
        // Started by `processes` below.
        say!("wintest: child process started");
        ExitProcess(7);
    }
    let mut checks = Checks::new("wintest");
    say!("wintest: command line: [{}]", command_line());
    relocations(&mut checks);
    environment_block(&mut checks);
    heap(&mut checks);
    files(&mut checks);
    console(&mut checks);
    processes(&mut checks);
    time(&mut checks);
    say!("wintest: all {} checks passed", checks.count());
    42
}
