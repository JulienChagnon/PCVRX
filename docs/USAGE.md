# Using OpenVRX: commands for every board and the app

All commands are for PowerShell on Windows. Paths assume the repo at
`D:\SOFTWARE DEV\OpenVRX`. Build first (once, and after code changes):

    cd "D:\SOFTWARE DEV\OpenVRX\host"; . ..\tools\env.ps1
    $env:CARGO_TARGET_DIR = "target\main"; cargo build --release -p openvrx; cargo build --release -p openvrx-hw --examples

Built programs land in `host\target\main\release\` (the app) and
`host\target\main\release\examples\` (the FX2 tools).

## The receiver app

    & "D:\SOFTWARE DEV\OpenVRX\host\target\main\release\openvrx.exe" --lanes 4x4top --clk 9 --mhz 5880

Only the FX2 goes on the PC's USB (FX2-only, the default). The C5 runs on
its own power with the settings it saved, so tell the app what those are:

| Option | Meaning |
|---|---|
| `--lanes PRESET` | lane layout the C5 saved: 8x8, 7x7, 6x6, 5x5, 4x4top, 4x4fine, 4x4ultra |
| `--clk DIV` | sample clock divider the C5 saved: 6 (40 MS/s), 9 (26.67), 12 (20), 18 (13.33) |
| `--mhz MHZ` | the C5's saved channel, for the OSD and recordings only (the app cannot read it in FX2-only mode) |
| `--control c5` | the C5 console is on USB too: the app tunes, sets gain, calibrates the sampling phase |
| `--hd3` | Fat Shark HD3 mode: fullscreen, 5:3 geometry, goggle-safe OSD |
| `--fullscreen` | start fullscreen |
| `--layout 1\|2\|4` | number of views |
| `--present immediate\|mailbox\|fifo` | swap chain mode (latency vs tearing) |
| `--source test` | built-in test patterns, no hardware |
| `--run-for SECONDS` | run, print latency statistics, exit |
| `--settings PATH` | settings file (default `%APPDATA%\OpenVRX\settings.json`) |

### Keys (F1 shows them in the app)

| Key | Action |
|---|---|
| F1 | help |
| Esc | the menu: channel, picture mode, every action with its key (closes help, panels and the recordings page first) |
| Tab | open/close the settings panel (receivers, video, recording, display) |
| O | OSD on/off |
| Left / Right | previous / next picture mode |
| R | start/stop recording |
| M | start/stop video recording (MP4 of view 1, needs ffmpeg) |
| B | the recordings page: play videos, run tools on a recording |
| H | HD3 mode |
| V | 1, 2 or 4 views |
| A | aspect (fit, fill, stretch) |
| S | scaling filter |
| D | damage overlay |
| P / L | present mode / present trigger |
| C | calibrate sampling phase (needs `--control c5`) |
| X | side by side: the live picture and the bench ESP32 test card it should show (`--compare-pattern NAME`, default card; `--compare` starts in it) |
| [ and ] | channel down/up (needs `--control c5`) |
| F5 | reset latency statistics |
| F11 | fullscreen |
| Ctrl+Q | quit |

The menu (Esc) opens at once on the left side of the window. Up/Down move
through its rows and Enter runs one, so it works without a mouse; a click
outside it closes it, and the other keys keep working while it is open. In
FX2-only mode a channel picked there is set on the C5 over its console when
the C5's USB is plugged in; the settings panel's channel and gain controls do
nothing in that mode (change them on the C5, below).

The OSD shows the picture mode at the top left at all times (amber for raw,
teal for the cleaned modes, amber with a warning when plugin mode has no
plugin loaded). A mode change also shows a strip near the bottom of the
window with the mode's description for under two seconds.

The UI font is the system's regular sans (Adwaita Sans, Inter, Noto Sans,
DejaVu Sans or Liberation Sans on Linux, Segoe UI on Windows) when one is
found; `OPENVRX_UI_FONT=FILE` picks another file, `OPENVRX_UI_FONT=builtin`
keeps egui's own.

### Recordings page (B)

The recording folder as a list, newest first: each raw recording with its
size, length and channel, and every video whose name starts with the
recording's name (the app's `NAME.mp4`, a tool's `NAME-clean.mp4`). Videos
with no raw recording are listed on their own. The folder is read one level
deep on a background thread; the refresh button reads it again.

Selecting an entry plays its first video in the window (ffmpeg and ffprobe
must be on the PATH). Keys while the page is open: Space play/pause,
Left/Right 5 s back/forward, `,` and `.` one frame back/forward, Home to the
start, Up/Down previous/next recording, Enter play, Esc or B back to the live
picture. The live picture keeps running behind the page and a recording in
progress goes on. Nothing on the page deletes or moves a file.

Tools are other programs the page can run on the selected recording: each
one is described in a JSON file in `tools.d` next to the settings file
(`$HOME/.config/openvrx/tools.d/` on Linux, `%APPDATA%\OpenVRX\tools.d\` on
Windows; F1 shows the path):

```json
{ "tools": [ {
    "name": "Render video",
    "description": "One line shown on hover",
    "program": "~/bin/my-renderer",
    "args": ["{input}", "{output}", "--crf", "18"],
    "input": "raw",
    "output_suffix": "-render.mp4"
} ] }
```

`input` is `raw` (`{input}` is the recording's `NAME.openvrx.json`) or `video`
(the video in the player). `{output}` is the input's name without extension
plus `output_suffix`, in the same folder; when that file exists `-2`, `-3`...
is added, so a tool never overwrites a file. `{base}` (the input without
extension) and `{dir}` are also filled in. `program` may start with `~/`, and
a relative path with a slash is taken from the JSON file's real folder
(symlinks followed). Tools run at low priority (`nice -n 10`, below normal on
Windows; `"low_priority": false` turns it off). Their output shows in the
jobs strip at the bottom of the page, with a progress bar when the tool
prints a percentage; Stop ends one, a finished one's Play opens its output.
Closing the app stops running tools.

Picture modes (the arrow keys, Esc menu or Tab, video): `raw` (demodulation,
sync and bob only), `low_latency` (the default: comb filter, repair,
edge-directed deinterlace), `balanced` (plus a light temporal denoise),
`quality` (3D comb, motion-compensated denoise), `learned` (the raw picture
through a learned denoise filter on the GPU) and `plugin` (a filter plugin
library, `--plugin`). The learned filter reads only the current and the two
previous fields, so it adds no delay; the app picks the filter that matches
the steady OSD setting (I). The arrow keys step through the list and wrap
around, so raw is one key away from low latency and from plugin.

## The C5 radio head (`tools/c5cmd.py`, `tools/flash.py`)

Plug the C5's USB-C into the PC (it shows as a COM port, COM9 here). Use
the ESP-IDF Python, which has pyserial:

    $py = "C:\Espressif\tools\python\v6.0.2\venv\Scripts\python.exe"
    & $py tools\c5cmd.py COM9 ver status pins

| Task | Command |
|---|---|
| set channel, layout, clock and keep them | `& $py tools\c5cmd.py COM9 "tune 5880" "lanes 4x4top" "clk 9" save status` |
| fixed gain (index, default 64) | `& $py tools\c5cmd.py COM9 "gain 60" save` |
| reboot | `& $py tools\c5cmd.py COM9 reboot` |
| flash new firmware | `idf.py build` in `firmware\c5`, then `& $py tools\flash.py COM9` |
| flash a board already in the ROM loader | `& $py tools\flash.py COM9 --no-dl` |

Never use `idf.py flash` or a serial tool that toggles DTR/RTS on a running
C5: the line reset hangs it until it is unplugged. `status` ends with
`boot=ok`, or `boot=<step>` when the previous boot stalled and the C5
started in safe mode on default settings. Full command list:
`docs/PROTOCOL.md` section 1. Raceband: R1 5658, R2 5695, R3 5732,
R4 5769, R5 5806, R6 5843, R7 5880, R8 5917.

## The FX2 bridge (tools in `host\target\main\release\examples\`)

The FX2 keeps its firmware in RAM; the app and these tools load it after
every plug-in.

| Tool | What it does |
|---|---|
| `fx2_probe.exe` | loads the firmware, runs a 5 s USB throughput test with the FX2's own clock |
| `fx2_diag.exe` | wiring check: control ties, data lanes, FIFO tests with the internal and C5 clock |
| `fx2_diag.exe --c5` | only the C5-clock stream test (run on a freshly plugged FX2) |
| `fx2_diag.exe --live` | pin levels every 0.5 s for 60 s, for re-seating wires |
| `fx2_diag.exe --find-ifclk` | finds the IFCLK header pin (jumper it to PD1) |

Wiring tables: `docs/HARDWARE.md`. Board pinouts: `docs/PINOUTS.md`.

## Offline and bench tools (`openvrx-cli`)

    openvrx-cli inspect REC [OUT_DIR]            lane activity, I/Q histograms, FM strip image
    openvrx-cli decode  REC [OUT_DIR] [FRAMES]   decoded frames as PNG
    openvrx-cli record  OUT_BASE --lanes 4x4top --clk 9 [--seconds 5] [--mhz 5880] [--gain 56]
    openvrx-cli score   REC [--ref REC] [--frames 30] [--skip 10]

`record` captures FX2-only without the app window; `score` prints one JSON
line (lock, C/N, clipping, frames, PSNR against a reference recording).
Attenuation sweeps built on them: `docs/BENCH.md`. Decoded frames and
recordings stay local.
