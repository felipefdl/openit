# System handoff

What OpenIt does with a file it has no reader for: hand the path to the operating system's default application for that type, instead of opening a window that reports a refusal. Extends [Open requests](application-architecture.md#open-requests).

## Scope

Covers routing at open time, the guard against a shell that points back at OpenIt, and process exit after a handoff. Does not cover the error surface for files OpenIt accepts and then fails on; that stays with the errors plan in the [application architecture](application-architecture.md#errors).

## Behavior

Every entry point that produces an open request takes part: `openit <path>...`, Cmd/Ctrl+O, copied files pasted with Cmd/Ctrl+N, and later Finder or Explorer open and drops. A handoff is silent: the file leaves for another application and OpenIt shows nothing for it.

### What is handed over

The reader's refusal decides, so the file is on disk and readable before anything leaves OpenIt:

| Refusal | Examples |
| --- | --- |
| The name has no reader in OpenIt | PDF until its reader lands, `.docx`, `.zip`, `.heic`, `.avif`, `.psd` |
| The bytes are not UTF-8 and the name claimed text | An executable or a database file named `.log` or with no extension |
| The file is over the reader's size cap | Text over 5 MB, images over 64 MB |

### What is not handed over

- The file cannot be read at all: missing, no permission, not a regular file. The default application would fail the same way, so the failure is logged and the error window arrives with the errors plan.
- OpenIt accepted the file and the decoder failed on the bytes. A truncated PNG is a broken file, not a routing decision; the image window opens and reports the failure in place.
- Clipboard text and clipboard images. There is no path to hand over.

### When the system has no handler

The platform reports it: macOS shows its own "no application" dialog, and the Linux and Windows shells answer in their own way. OpenIt does not duplicate that message. The platform call is fire and forget; GPUI logs a failed invocation.

### Repeat guard

A shell can point back at OpenIt. Make OpenIt the default application for Markdown, then open a 12 MB Markdown file: the size cap refuses it, the handoff asks the shell to open it, and the shell hands it back as a new open request.

OpenIt records every handed-off path and refuses a second handoff of the same path within 10 seconds. The refusal is logged and ends the cycle. A deliberate reopen after that window behaves normally. The record is per process, which covers the shells that reuse a running instance (LaunchServices, and the local socket the command line will use).

### Exit after a handoff

`openit paper.pdf` must not leave an event loop running with nothing on screen. When an open request ends in a handoff, no other open request is in flight, and no window is open, OpenIt quits. Draft restore and the remaining command-line paths count as requests in flight, so a launch that mixes a PDF with a recovered draft keeps running.

A launch with no paths does not quit: nothing was handed over, and the empty window is a document session of its own.

The exit waits briefly. The platform spawns the system opener on its own background task, and quitting in the same tick can drop it before the process starts.

## Design

| Piece | Responsibility |
| --- | --- |
| `openit_core::handoff` | Which reader refusals hand over, and the repeat guard. No UI, no platform calls. |
| `openit::handoff` | The `SystemOpener` seam, the platform implementation over `App::open_with_system`, the application global holding the opener, the guard, and the in-flight count. |
| `openit::window` | Counts each open request and routes its failure to the handoff. |

The seam is required rather than convenient: GPUI's test platform panics on `open_with_system`, so a test that reaches the real call fails on the platform, not on the behavior.

## Testing

- Document library: the refusals that hand over and the ones that do not; the guard refusing a repeat inside the window and admitting the same path after it.
- Desktop application: an unsupported name and a non-UTF-8 file both open no window and reach the recording opener with the canonical path; an unreadable file reaches neither.
- Exit after a handoff is verified by running the binary on an unsupported file.

## References

- GPUI `App::open_with_system`: `open --` on macOS, `xdg-open` on Linux, `ShellExecute` on Windows.

## History

- 2026-09-09: Written and implemented. Reader refusals route to the system opener, with a 10 second repeat guard and exit when a handoff leaves nothing on screen.
