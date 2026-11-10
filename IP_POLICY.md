# Intellectual-property hygiene

How this project keeps its code clean of other people's. It applies to
every change, whoever or whatever writes it — most of the code so far
was written by an AI coding agent under the direction of the project's
maintainer. This is a working policy, not legal advice.

## Windows application compatibility

Where: `compat/windows/` (the PE reader, helper libraries, the Windows
test programs) and `userspace/programs/src/bin/winrun/` (the loader and
the implementation of the Windows functions). It is our own
implementation, written from scratch. It is not a port of Wine.

- **Implement only from** public documentation — Microsoft Learn and
  the Win32 API reference, the published PE/COFF specification, open
  standards — and from the black-box behaviour of test programs.
- **Never use, reproduce or paraphrase** leaked, NDA or shared-source
  Windows code (the Windows Research Kernel included), or decompiled or
  disassembled Microsoft binaries.
- **Do not copy or closely mirror Wine, ReactOS or any other
  reimplementation of Win32.** If a specific piece is ever deliberately
  imported from one of them, it must keep its original copyright and
  SPDX lines (Wine: LGPL-2.1-or-later; ReactOS: GPL-2.0-or-later), be
  listed in `PROVENANCE.md`, and be approved by the maintainer first.
  (Note that neither licence may be linked into the kernel side, and
  LGPL-2.1 code needs care anywhere; see `COPYING`.)
- **Every source file there starts with a `Sources:` comment** saying
  what it was written from and which tests pin its behaviour, e.g.
  `// Sources: Microsoft Learn "CreateFileW"; PE Format specification.
  Behaviour tests: wintest.exe`. CI checks that the comment exists.
- **Behaviour is pinned by tests that run real programs**: the Windows
  executables in `compat/windows/samples/`, built here and run by the
  QEMU tests `windows-programs` and `desktop`, and synthetic images in
  the unit tests of `compat/windows/pe`. A function without a test is
  not finished.
- **Windows names are used only where programs depend on them**:
  function names, structure layouts, constants and error codes of the
  public API and ABI. Internal structure and naming are our own.

One known grey area, recorded rather than hidden: four fields of the
thread and process blocks that programs' compiled code may read
directly (process ID, thread ID and last-error value in the TEB; image
base in the PEB) are at offsets that Microsoft's reference pages do not
name. They are long-published facts of the x64 ABI, used here because
programs depend on them, and are on the list for legal review.

## Linux driver compatibility

Where: imported Linux files live in `drivers/linux/` and nowhere else.
The compatibility layer they are compiled against — headers in
`compat/linux-kpi/`, runtime in `kernel/src/linuxkpi/` — is our own code
and contains no Linux source.

- Imported files are kept **unmodified**, with their upstream copyright
  and SPDX lines, and each is listed in `PROVENANCE.md` with its
  upstream path, kernel version or commit, and licence. CI checks the
  listing.
- The compatibility headers declare interfaces by the names Linux
  drivers use; they are written from the documented behaviour of those
  interfaces, not by copying Linux headers.
- Imported Linux code and Windows-compatibility code never share a file
  or a directory.

## Rules for AI-written code

- No large verbatim blocks recalled from any third-party codebase. If a
  piece of code turns out to be closely based on a specific open-source
  project, the agent stops and tells the maintainer before committing.
- Each commit message says what the human maintainer decided or
  changed — the design decisions, the instructions, the review edits.
  Human authorship matters for copyright, and so for the enforceability
  of the licences.

## User interface and assets

- Icons, wallpapers, logos, sounds and cursors are original or under a
  compatible open licence, and recorded in `REUSE.toml` and
  [`ATTRIBUTION.md`](ATTRIBUTION.md).
- Features are not named after another vendor's branded features, and
  nothing implies affiliation; see "Names and assets" in
  [`CONTRIBUTING.md`](CONTRIBUTING.md) and
  [`TRADEMARKS.md`](TRADEMARKS.md).
- Nothing is extracted from Windows or macOS binaries.
- No proprietary fonts (Segoe UI, SF Pro and the like) are bundled.
- Interaction patterns may be inspired by other systems; visual designs
  are not traced or copied.

## If contamination is suspected

1. Freeze merges that touch the affected files.
2. Check provenance: git history, `PROVENANCE.md`, the `Sources:`
   comments.
3. If the code cannot be cleared, remove it and reimplement from
   permitted sources — by someone, or a session, that has not read the
   suspect code.
