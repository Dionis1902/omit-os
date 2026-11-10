// SPDX-License-Identifier: GPL-2.0-only
//! Exercises the Windows API functions the compatibility layer provides
//! and checks each result, including the documented error codes. Prints
//! one line per group of checks and returns exit code 42 from the entry
//! point (rather than calling `ExitProcess`) when everything passed.

#![no_std]
#![no_main]

use core::arch::asm;
use core::ffi::CStr;
use core::hint::black_box;
use core::ptr::null_mut;

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
        checks.check(
            both == INVALID_HANDLE_VALUE && GetLastError() == 50,
            "read+write refused",
        );

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

fn time(checks: &mut Checks) {
    let before = GetTickCount64();
    Sleep(50);
    let elapsed = GetTickCount64() - before;
    checks.check((40..2000).contains(&elapsed), "Sleep(50) takes about 50 ms");
    say!("wintest: time ok");
}

/// Entry point.
#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> u32 {
    let mut checks = Checks::new("wintest");
    say!("wintest: command line: [{}]", command_line());
    relocations(&mut checks);
    environment_block(&mut checks);
    heap(&mut checks);
    files(&mut checks);
    console(&mut checks);
    time(&mut checks);
    say!("wintest: all {} checks passed", checks.count());
    42
}
