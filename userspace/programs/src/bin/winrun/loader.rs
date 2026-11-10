// SPDX-License-Identifier: GPL-2.0-only
//! Loading Windows images — the program and the libraries (DLLs) it uses —
//! into this process.
//!
//! Loading an image means: validate the file, map it at an address the
//! kernel chooses, apply base relocations, point each imported function
//! at its implementation, and give every section the access rights it
//! declares. An import from `KERNEL32.dll` is bound to this program's own
//! implementation ([`crate::kernel32`]); an import from any other library
//! makes the loader look for a file of that name — next to the importing
//! image, then in `/bin` — and load it the same way, recursively.
//!
//! Nothing in a loaded image runs until everything is resolved: a missing
//! library or function fails the whole load, and every missing item is
//! reported. Libraries are then initialised (their entry point is called
//! with `DLL_PROCESS_ATTACH`) in dependency order, by the caller.
//!
//! Libraries are never unloaded: `FreeLibrary` succeeds without effect.
//! That is permitted behaviour (Windows also keeps "pinned" libraries)
//! and avoids having to prove that no code or data of the library is
//! still in use.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use omit_pe::{ExportTarget, Pe, SUBSYSTEM_CONSOLE, SUBSYSTEM_GUI, Symbol};
use omit_rt::abi::protect;
use omit_rt::sync::Mutex;

use crate::kernel32;

const PAGE: usize = 4096;

/// A loaded image.
struct Module {
    /// File name in lower case, such as `sample.dll`: what imports and
    /// `GetModuleHandle` find it by.
    name: String,
    /// Where it was loaded from (an Omit OS path).
    path: String,
    base: usize,
    size: usize,
    /// Exported functions: name (if any), ordinal, address.
    exports: Vec<(Option<String>, u32, usize)>,
}

/// The loaded images; the program itself is the first. The lock is not
/// held while code of a loaded image runs, and loading takes it in short
/// steps: two threads calling `LoadLibrary` for the same new library at
/// the same moment could both load it. Programs serialise library
/// loading themselves in practice; this is a known gap.
static MODULES: Mutex<Vec<Module>> = Mutex::new(Vec::new());

fn with_modules<R>(f: impl FnOnce(&mut Vec<Module>) -> R) -> R {
    f(&mut MODULES.lock())
}

/// Why a load failed.
pub enum LoadError {
    /// The file is missing, malformed or of an unsupported kind.
    Unusable(String),
    /// Functions or libraries the image needs do not exist; each entry
    /// describes one.
    Missing(Vec<String>),
}

fn unusable(path: &str, reason: impl core::fmt::Display) -> LoadError {
    LoadError::Unusable(format!("{path}: {reason}"))
}

/// An image whose initialisation routine still has to be called.
pub struct Initialiser {
    /// Address of the library's entry point (`DllMain`).
    pub entry: usize,
    /// The library's base address, passed to the entry point.
    pub base: usize,
    /// For error messages.
    pub name: String,
}

/// The result of loading the program.
pub struct Program {
    pub base: usize,
    pub entry: usize,
    pub stack_reserve: u64,
    /// The libraries the program needs, dependencies first.
    pub initialisers: Vec<Initialiser>,
}

/// The file-name part of a path, in lower case.
fn module_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase()
}

/// Loads the program at `path` and every library it needs.
pub fn load_program(path: &str) -> Result<Program, LoadError> {
    let mut initialisers = Vec::new();
    let mut missing = Vec::new();
    let loaded = load_image(path, false, &mut missing, &mut initialisers)?;
    if !missing.is_empty() {
        return Err(LoadError::Missing(missing));
    }
    Ok(Program {
        base: loaded.base,
        entry: loaded.entry,
        stack_reserve: loaded.stack_reserve,
        initialisers,
    })
}

/// Loads the library `name` (a file name, with `.dll` assumed if it has
/// no extension, or an Omit OS path) unless it is loaded already. Returns
/// its base address and the libraries that now need initialising. On
/// failure nothing stays loaded.
pub fn load_library(name: &str) -> Result<(usize, Vec<Initialiser>), LoadError> {
    let name = with_extension(name);
    if let Some(base) = find(&name) {
        return Ok((base, Vec::new()));
    }
    let Some(path) = locate(&name, None) else {
        return Err(LoadError::Missing(alloc::vec![format!(
            "{name} (library not found)"
        )]));
    };
    let already = with_modules(|modules| modules.len());
    let mut initialisers = Vec::new();
    let mut missing = Vec::new();
    let result = load_image(&path, true, &mut missing, &mut initialisers);
    let failure = match result {
        Ok(loaded) if missing.is_empty() => return Ok((loaded.base, initialisers)),
        Ok(_) => LoadError::Missing(missing),
        Err(error) => error,
    };
    // Undo: no code of these images has run and nothing refers to them.
    let abandoned = with_modules(|modules| modules.split_off(already));
    for module in abandoned {
        // SAFETY: the image was mapped by `load_image` during this call
        // and never entered.
        let _ = unsafe { omit_rt::unmap(module.base as *mut u8, module.size) };
    }
    Err(failure)
}

/// `name` with `.dll` appended if its last component has no extension.
fn with_extension(name: &str) -> String {
    let file = name.rsplit('/').next().unwrap_or(name);
    if file.contains('.') {
        name.into()
    } else {
        format!("{name}.dll")
    }
}

/// The base address of the loaded image called `name` (compared without
/// regard to case, with or without a directory).
pub fn find(name: &str) -> Option<usize> {
    let wanted = module_name(&with_extension(name));
    with_modules(|modules| {
        modules
            .iter()
            .find(|module| module.name == wanted)
            .map(|module| module.base)
    })
}

/// The path of the image loaded at `base`.
pub fn path_of(base: usize) -> Option<String> {
    with_modules(|modules| {
        modules
            .iter()
            .find(|module| module.base == base)
            .map(|module| module.path.clone())
    })
}

/// The address of the function `symbol` exported by the image at `base`.
pub fn export(base: usize, symbol: Symbol<'_>) -> Option<usize> {
    with_modules(|modules| {
        let module = modules.iter().find(|module| module.base == base)?;
        module
            .exports
            .iter()
            .find(|(name, ordinal, _)| match symbol {
                Symbol::Name(wanted) => name.as_deref() == Some(wanted),
                Symbol::Ordinal(wanted) => *ordinal == u32::from(wanted),
            })
            .map(|&(_, _, address)| address)
    })
}

/// Finds the file for library `name`: as a path if it contains a `/`,
/// otherwise next to the image that needs it (`beside`), then in `/bin`.
/// Since file names are case-sensitive here and library names on Windows
/// are not, the name is tried as written and in lower case.
fn locate(name: &str, beside: Option<&str>) -> Option<String> {
    let exists = |path: &str| omit_rt::stat(path).is_ok_and(|stat| stat.kind == 1);
    if name.contains('/') {
        return exists(name).then(|| name.into());
    }
    let directory = beside
        .and_then(|path| path.rfind('/').map(|index| &path[..index]))
        .unwrap_or(".");
    let lower = name.to_ascii_lowercase();
    for directory in [directory, "/bin"] {
        for candidate in [name, lower.as_str()] {
            let path = format!("{directory}/{candidate}");
            if exists(&path) {
                return Some(path);
            }
        }
    }
    None
}

struct Loaded {
    base: usize,
    entry: usize,
    stack_reserve: u64,
}

/// Maps and binds one image, loading the libraries it imports first.
/// Missing functions and libraries are added to `missing`; the image is
/// then left registered but must not be run.
fn load_image(
    path: &str,
    as_library: bool,
    missing: &mut Vec<String>,
    initialisers: &mut Vec<Initialiser>,
) -> Result<Loaded, LoadError> {
    let file = omit_rt::read_file(path).map_err(|error| unusable(path, error))?;
    let pe = Pe::parse(&file).map_err(|error| unusable(path, error))?;

    if pe.is_dll() != as_library {
        return Err(unusable(
            path,
            if as_library {
                "this is a program, not a library"
            } else {
                "this is a library, not a program"
            },
        ));
    }
    let features = pe.features();
    if features.dotnet {
        return Err(unusable(path, ".NET code is not supported"));
    }
    if !as_library {
        match pe.subsystem() {
            // Graphical programs differ from console programs only in
            // what they import; whether that is available is checked
            // function by function below.
            SUBSYSTEM_CONSOLE | SUBSYSTEM_GUI => {}
            other => {
                return Err(unusable(
                    path,
                    format!("not a console or graphical program (subsystem {other})"),
                ));
            }
        }
    }
    if features.tls {
        return Err(unusable(
            path,
            "it uses thread-local storage, which is not supported",
        ));
    }
    if features.delay_imports {
        return Err(unusable(
            path,
            "it uses delay-loaded libraries, which are not supported",
        ));
    }
    if pe.relocations_stripped() {
        return Err(unusable(
            path,
            format!(
                "it has no relocation information and only runs at address {:#x}, which is \
                 not available",
                pe.image_base()
            ),
        ));
    }
    if let Some(section) = pe.sections().iter().find(|s| s.writable && s.executable) {
        return Err(unusable(
            path,
            format!("section {} is both writable and executable", section.name()),
        ));
    }

    let size = pe.size_of_image() as usize;
    // Windows places images at multiples of 64 KiB; programs may rely on
    // that.
    let base = omit_rt::map(size, 0x1_0000)
        .map_err(|error| unusable(path, format!("cannot allocate {size} bytes: {error}")))?;
    // From here on the image is registered, so that a failure further
    // down (in this image or one it imports) can be cleaned up by the
    // caller, and so that libraries importing each other find each other.
    let mut exports = Vec::new();
    let listed = pe.exports(|export| {
        // A forwarded export names a function in another library; those
        // are not followed, so it is as if the export did not exist.
        if let ExportTarget::Rva(rva) = export.target {
            exports.push((
                export.name.map(String::from),
                export.ordinal,
                base as usize + rva as usize,
            ));
        }
    });
    with_modules(|modules| {
        modules.push(Module {
            name: module_name(path),
            path: path.into(),
            base: base as usize,
            size,
            exports,
        });
    });
    listed.map_err(|error| unusable(path, error))?;

    let mut imports = Vec::new();
    pe.imports(|import| imports.push(import))
        .map_err(|error| unusable(path, error))?;
    let mut bindings = Vec::with_capacity(imports.len());
    for import in imports {
        let address = if import.library.eq_ignore_ascii_case("kernel32.dll") {
            kernel32::resolve(import.symbol)
        } else if let Some(address) = crate::gui::resolve(import.library, import.symbol) {
            address
        } else {
            let library = match find(import.library) {
                Some(library) => Some(library),
                None => match locate(import.library, Some(path)) {
                    Some(library_path) => {
                        Some(load_image(&library_path, true, missing, initialisers)?.base)
                    }
                    None => None,
                },
            };
            match library {
                Some(library) => export(library, import.symbol),
                None => {
                    let note = format!("{} (library not found)", import.library);
                    if !missing.contains(&note) {
                        missing.push(note);
                    }
                    continue;
                }
            }
        };
        match address {
            Some(address) => bindings.push((import.slot_rva as usize, address)),
            None => missing.push(format!("{}!{}", import.library, import.symbol)),
        }
    }

    {
        // SAFETY: `map` returned `size` bytes of zeroed, writable memory
        // that nothing else refers to; the slice is dropped before the
        // memory is used in any other way.
        let image = unsafe { core::slice::from_raw_parts_mut(base, size) };
        pe.build_image(image)
            .and_then(|()| omit_pe::relocate(&pe, image, base as u64))
            .map_err(|error| unusable(path, error))?;
        for &(slot, address) in &bindings {
            // The parser checked that every slot lies inside a section.
            image[slot..slot + 8].copy_from_slice(&(address as u64).to_le_bytes());
        }
    }

    // Access rights: everything read-only, then each section as declared.
    // No memory is ever writable and executable.
    let set = |offset: usize, length: usize, protection: u64| {
        let length = length.next_multiple_of(PAGE);
        // SAFETY: the range lies inside the image mapped above, and no
        // reference into the image is live.
        unsafe { omit_rt::protect(base.wrapping_add(offset), length, protection) }
            .map_err(|error| unusable(path, format!("cannot protect memory: {error}")))
    };
    set(0, size, protect::READ)?;
    for section in pe.sections() {
        let protection = match (section.writable, section.executable) {
            (true, _) => protect::WRITE,
            (false, true) => protect::EXECUTE,
            (false, false) => continue,
        };
        set(
            section.rva as usize,
            section.virtual_size as usize,
            protection,
        )?;
    }

    let entry = base as usize + pe.entry_rva() as usize;
    if as_library && pe.entry_rva() != 0 {
        // After the libraries this one imports, which the recursion above
        // has already added.
        initialisers.push(Initialiser {
            entry,
            base: base as usize,
            name: module_name(path),
        });
    }
    Ok(Loaded {
        base: base as usize,
        entry,
        stack_reserve: pe.stack_reserve(),
    })
}
