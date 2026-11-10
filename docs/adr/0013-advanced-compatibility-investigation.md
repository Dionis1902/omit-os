# ADR 0013: Advanced compatibility — what was investigated, what was built

- Status: accepted (as a record of findings; the plan is a proposal)
- Date: 2026-11-10

## Context

Milestone 9 of the project plan asks to *investigate and implement*:
broader Windows API support, Direct3D compatibility, Vulkan integration,
advanced applications, game compatibility, additional Linux driver
families, GPU performance optimisation, and latency and frame pacing
improvements. The plan itself warns that these are long-term objectives
that must not be presented as straightforward.

This record separates three things: what was actually built and verified
in this milestone, what was measured, and what was only investigated —
with the reasons why it cannot be built yet and the order in which it
could be.

## Built and verified

- **Broader Windows API: libraries.** Windows programs can use DLL files:
  export tables, recursive loading with dependency-ordered
  initialisation, `LoadLibraryA`, `GetProcAddress` and related calls
  (ADR 0011). Verified with two DLLs and 33 self-checks.
- **Additional Linux driver family.** A second unmodified Linux driver,
  `pvpanic`, from a different subsystem than the first (ADR 0010). It
  forced real additions to the native kernel — PCI resource assignment
  and a panic hook — not just to the compatibility layer.
- **Latency.** Input-to-screen latency is now measured, and one cause of
  it was removed (below).
- **A correctness bug found by broader testing.** Running CPU-bound
  programs under KVM exposed a wrong stack-segment selector after
  `sysret` on AMD CPUs (ADR 0008, addendum).

## Measured: input-to-screen latency

Input events now carry the time at which the kernel received them from
the device; the desktop reports, when it exits, the time from that moment
to the completed screen update. The automated desktop test (55 screen
updates: pointer movement, clicks, typing, a window drag) was run three
times before and after one change, with a release build under QEMU's
software emulation on the development machine:

| | average | longest |
| --- | --- | --- |
| input checked once per 10 ms timer tick | 10.0 – 16.5 ms | 22.7 – 32.5 ms |
| reader woken by the interrupt handler | 2.6 – 3.6 ms | 9.2 – 57.6 ms |

The average fell by roughly the expected half tick plus scheduling delay.
The longest case did not improve and varies widely between runs; it has
not been analysed (candidates: large repaints, and the host scheduling the
emulator). These are emulation numbers on a shared machine: they show the
direction of the change, not what a user would see on hardware.

**Frame pacing against the display does not exist**, and cannot with the
current display path: the firmware framebuffer has no vertical-blank
signal, so there is nothing to synchronise with, and updates are copied
as soon as they are drawn. Tearing is possible. Timers still have 10 ms
resolution.

*Update — pacing against the clock, measured.* The first program that
redraws continuously, the game `/bin/pop`, schedules each frame a fixed
20 ms after the previous one was due and reports on exit how evenly its
frames were actually shown. In the `desktop` test (QEMU software
emulation, the game's window composited by the desktop, about five
seconds of play): release build, 269 frames, average interval 20.0 ms,
longest 25.2 ms, none more than half a frame late; debug build, 282
frames, average 20.0 ms, longest 29.7 ms, none late. So a steady 50
frames per second is achievable through the whole path — program,
shared memory, desktop, kernel blit. Two limits are structural: frame
times can only be multiples of the 10 ms timer tick (60 Hz cannot be
paced evenly), and nothing ties a frame to the screen's refresh. As
with the latency figures, these are emulation numbers.

## Investigated only: Direct3D, Vulkan, games, GPU performance

None of this is implemented. What follows is what would be required.

**How Direct3D runs outside Windows today.** Direct3D is translated to
Vulkan (or OpenGL) in user space: DXVK for Direct3D 8–11, vkd3d for
Direct3D 12, WineD3D inside Wine. These are large, mature projects. They
are libraries that expect, underneath them: a complete Vulkan
implementation, the Win32 window system to present into, a C++ runtime,
threads, thread-local storage and structured exception handling, and a
large part of the base Windows API. Omit OS has none of those yet.

**What a Vulkan implementation needs.** On open systems Vulkan is
provided by Mesa's drivers, in user space, on top of kernel graphics
drivers (Linux DRM). The user-space half needs a C library, threads,
shared libraries and a C++ runtime. The kernel half is the hardest driver
family there is: memory managers shared between CPU and GPU, DMA,
interrupts, work queues, timers, firmware loading, power management,
display mode setting. FreeBSD reuses the Linux GPU drivers through its
LinuxKPI; that effort is many times the size of this entire repository
and has taken a team years. The two drivers reused here (about 640 lines,
no interrupts, no DMA) say nothing about its feasibility except that the
mechanism works.

**What games need beyond graphics.** Audio, controller input, networking,
the registry, COM, often .NET, launchers that embed a web browser, and in
many cases copy protection or anti-cheat components that install Windows
kernel drivers. The last category cannot work on any system that is not
Windows and should be expected never to.

**A policy question for the project owner.** The project rules exclude
Wine and Proton. DXVK and vkd3d are separate projects with their own
licences (zlib and LGPL respectively) that Proton happens to bundle.
Whether translating Direct3D with them is acceptable, or whether a
Direct3D implementation must be written from the specification, changes
the size of the task by years. This record does not decide it.

**GPU performance optimisation.** There is no GPU code to optimise.
Nothing is claimed.

## Proposed order of work

Each step is independently verifiable, and each is a prerequisite for the
next. None is small.

1. **Processes that can cooperate**: pipes and shared memory, then
   threads within a process, thread-local storage. (Also what the desktop
   needs to become a display server, and what Windows programs with a C
   runtime need.)
2. **A C library and dynamic linking for native programs**, so that
   existing C and C++ libraries can be ported instead of rewritten.
3. **A display driver with real mode setting and vertical-blank events**:
   VirtIO GPU in 2D mode, natively. This is what makes frame pacing
   implementable and measurable — still without 3D.
4. **Vulkan in a virtual machine first**: VirtIO GPU can forward Vulkan
   to the host ("Venus"), which needs the guest's user-space driver and
   kernel support for shared buffers, but no hardware GPU driver. This
   would be the first point at which a Vulkan program could draw a
   triangle on Omit OS.
5. **The Win32 window system on the Omit OS display server**, and with it
   the decision above about Direct3D translation.
6. **Hardware GPU drivers** through LinuxKPI, starting with the simplest
   display-only driver to grow the DRM support code, long before any 3D
   driver.

On memory protection: the kernel forbids memory that is writable and
executable at the same time. Shader compilers and other code generators
remain possible under that rule, because a program may switch a region
from writable to executable (`protect`); they need porting to that
pattern, not an exception to the rule.

## Consequences

- The README's status table lists Direct3D, Vulkan, games and GPU
  acceleration as not started, and will continue to until a test shows
  otherwise.
- The next milestone-sized piece of work is step 1.
