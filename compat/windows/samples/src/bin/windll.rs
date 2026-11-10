// SPDX-License-Identifier: GPL-2.0-only
//! Tests libraries (DLLs): one imported by the program, `winsample.dll`,
//! and one loaded on request, `winplugin.dll`, which itself needs the
//! first.

#![no_std]
#![no_main]

use core::ffi::{CStr, c_char, c_void};
use core::ptr::null_mut;

use omit_win_samples::*;

#[link(name = "winsample", kind = "raw-dylib")]
unsafe extern "system" {
    safe fn sample_add(a: i32, b: i32) -> i32;
    safe fn sample_attach_count() -> u32;
    safe fn sample_loaded_with_program() -> u32;
    safe fn sample_module() -> *mut c_void;
    safe fn sample_greeting(index: u32) -> *const c_char;
    fn sample_print(text: *const c_char) -> i32;
}

fn imported_library(checks: &mut Checks) {
    // The library was initialised, once, before the program started.
    checks.check(sample_attach_count() == 1, "library initialised once");
    checks.check(sample_loaded_with_program() == 1, "loaded with the program");
    checks.check(sample_add(40, 2) == 42, "call through the import table");
    // SAFETY: the library returns NUL-terminated strings or null.
    unsafe {
        let greeting = CStr::from_ptr(sample_greeting(0));
        checks.check(greeting == c"greetings from winsample.dll", "library data");
        checks.check(
            CStr::from_ptr(sample_greeting(1)) == c"second greeting",
            "library data",
        );
        checks.check(sample_greeting(2).is_null(), "library bounds check");
        checks.check(
            sample_print(c"printed by the library through its own imports".as_ptr()) == 1,
            "library calls KERNEL32",
        );
    }
}

fn module_functions(checks: &mut Checks) {
    // SAFETY: NUL-terminated names and live buffers throughout; function
    // pointers obtained from `GetProcAddress` are called with the
    // signatures the libraries define.
    unsafe {
        let library = GetModuleHandleA(c"winsample.dll".as_ptr());
        checks.check(
            !library.is_null() && library == sample_module(),
            "GetModuleHandleA",
        );
        checks.check(
            GetModuleHandleA(c"WINSAMPLE".as_ptr()) == library,
            "name matching",
        );
        checks.check(
            GetModuleHandleA(c"notloaded.dll".as_ptr()).is_null() && GetLastError() == 126,
            "ERROR_MOD_NOT_FOUND",
        );
        checks.check(
            LoadLibraryA(c"winsample.dll".as_ptr()) == library,
            "LoadLibraryA again",
        );
        checks.check(sample_attach_count() == 1, "not initialised twice");

        let add = GetProcAddress(library, c"sample_add".as_ptr());
        checks.check(!add.is_null(), "GetProcAddress");
        let add: extern "system" fn(i32, i32) -> i32 = core::mem::transmute(add);
        checks.check(add(2, 3) == 5, "call through GetProcAddress");
        checks.check(
            GetProcAddress(library, c"no_such_function".as_ptr()).is_null()
                && GetLastError() == 127,
            "ERROR_PROC_NOT_FOUND",
        );
        checks.check(
            GetProcAddress(0x1234_0000 as *mut c_void, c"sample_add".as_ptr()).is_null()
                && GetLastError() == 126,
            "GetProcAddress with a bad module",
        );

        // KERNEL32 itself can be asked for functions.
        let kernel = GetModuleHandleA(c"kernel32.dll".as_ptr());
        checks.check(!kernel.is_null(), "kernel32 module handle");
        let ticks = GetProcAddress(kernel, c"GetTickCount64".as_ptr());
        checks.check(!ticks.is_null(), "GetProcAddress(kernel32)");
        let ticks: extern "system" fn() -> u64 = core::mem::transmute(ticks);
        checks.check(ticks() > 0, "call a kernel32 function by address");
        checks.check(
            GetProcAddress(kernel, c"CreateThread".as_ptr()).is_null() && GetLastError() == 127,
            "missing kernel32 function",
        );

        let mut buffer = [0u8; 64];
        let length = GetModuleFileNameA(null_mut(), buffer.as_mut_ptr(), 64);
        let name = core::str::from_utf8(&buffer[..length as usize]).unwrap_or("?");
        say!("windll: program file: {name}");
        checks.check(name.ends_with("\\windll.exe"), "GetModuleFileNameA(NULL)");
        let length = GetModuleFileNameA(library, buffer.as_mut_ptr(), 64);
        let name = core::str::from_utf8(&buffer[..length as usize]).unwrap_or("?");
        checks.check(
            name.ends_with("\\winsample.dll"),
            "GetModuleFileNameA(library)",
        );
        SetLastError(0);
        let length = GetModuleFileNameA(library, buffer.as_mut_ptr(), 8);
        checks.check(
            length == 8 && buffer[7] == 0 && GetLastError() == 122,
            "ERROR_INSUFFICIENT_BUFFER",
        );
    }
}

fn library_on_request(checks: &mut Checks) {
    // SAFETY: as in `module_functions`.
    unsafe {
        checks.check(
            GetModuleHandleA(c"winplugin.dll".as_ptr()).is_null(),
            "plugin is not loaded yet",
        );
        checks.check(
            LoadLibraryA(c"nosuch.dll".as_ptr()).is_null() && GetLastError() == 126,
            "LoadLibraryA of a missing file",
        );
        // Without the extension, as programs often write it.
        let plugin = LoadLibraryA(c"winplugin".as_ptr());
        checks.check(!plugin.is_null(), "LoadLibraryA");
        checks.check(
            GetModuleHandleA(c"winplugin.dll".as_ptr()) == plugin,
            "plugin handle",
        );

        let lookup = |name: &CStr| GetProcAddress(plugin, name.as_ptr());
        let how_loaded: extern "system" fn() -> u32 =
            core::mem::transmute(lookup(c"plugin_how_loaded"));
        let dependency_ready: extern "system" fn() -> u32 =
            core::mem::transmute(lookup(c"plugin_dependency_ready"));
        let compute: extern "system" fn(i32) -> i32 =
            core::mem::transmute(lookup(c"plugin_compute"));
        checks.check(how_loaded() == 2, "plugin initialised as loaded on request");
        checks.check(dependency_ready() == 1, "its dependency was ready first");
        // The plugin calls into winsample.dll, which is the same instance
        // this program uses.
        checks.check(compute(5) == 105, "library calling another library");
        checks.check(sample_attach_count() == 1, "dependency not loaded twice");

        checks.check(FreeLibrary(plugin) != 0, "FreeLibrary");
        checks.check(
            FreeLibrary(0x1234_0000 as *mut c_void) == 0 && GetLastError() == 126,
            "FreeLibrary with a bad module",
        );
    }
}

/// Entry point.
#[unsafe(no_mangle)]
pub extern "C" fn mainCRTStartup() -> u32 {
    let mut checks = Checks::new("windll");
    imported_library(&mut checks);
    module_functions(&mut checks);
    library_on_request(&mut checks);
    say!("windll: all {} checks passed", checks.count());
    0
}
