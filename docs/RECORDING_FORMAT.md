# OpenVRX recording format (version 1)

A recording keeps the raw lane words exactly as the FX2 delivered them, so
it can be decoded again later with better code. The simulator writes the
same format, so every tool runs on simulated and real captures alike.

## Files

For a recording named `NAME`:

| File | Content |
|---|---|
| `NAME.openvrx.json` | Sidecar: everything needed to decode (written at start, rewritten at close) |
| `NAME.rx0.bin`, `NAME.rx1.bin`, ... | Raw bytes per receiver, exactly as received |
| `NAME.events.jsonl` | One JSON object per line, appended live (crash-safe) |
| `NAME.truth.f32` | Simulator only: clean composite video, little-endian f32 IRE, one value per *input sample* of rx0 (see `truth`) |

Opening `NAME.openvrx.json` (or any of the files) opens the recording.

## Sidecar

```json
{
  "format": "openvrx-recording",
  "version": 1,
  "created_utc": "2026-10-29T21:14:03Z",
  "software": "openvrx 0.1.0",
  "source": "hardware",
  "notes": "",
  "closed_cleanly": true,
  "receivers": [
    {
      "rx": 0,
      "file": "NAME.rx0.bin",
      "bytes": 1600000000,
      "sample_format": {
        "preset": "7x7",
        "lanes": { "word_bytes": 2, "encoding": { "kind": "iq",
          "i": { "word_msb": 15, "adc_bits": [9,8,7,6,5,4,3] },
          "q": { "word_msb": 7,  "adc_bits": [9,8,7,6,5,4,3] } } },
        "divider": 18
      },
      "sample_rate_hz": 13333333.333,
      "channel_mhz": 5732,
      "offset_khz": 0,
      "gain": 64,
      "p11": true,
      "board": "xiao",
      "c5_fw": "0.1.0",
      "fx2_fw": "0.1.0",
      "fx2_serial": "A1B2C3D4",
      "ifclk_inverted": false,
      "watch_n": 1,
      "start_host_ns": 123456789,
      "antenna": "omni RHCP"
    }
  ],
  "standard_hint": "ntsc",
  "sim": null
}
```

`sample_format` is serde's form of `openvrx_core::SampleFormat`.

For simulated recordings `source` is `"sim"` and `sim` holds the generator
settings (C/N, fading, deviation, standard, seeds, picture source) plus:

```json
"truth": { "file": "NAME.truth.f32", "rx": 0, "unit": "ire",
           "delay_samples": 0.0 }
```

`delay_samples` is the group delay the simulator's channel adds, so a
decoder output at input sample `n` compares with `truth[n - delay]`.

### Context (optional)

`context` holds what the recording program knew besides the signal. Nothing
in it is needed to decode, its keys are free form (each program owns its
own) and it is left out when empty. Readers must not depend on it.

| key | written by | content |
|---|---|---|
| `build` | app, CLI | `software`, `commit` (short hash of HEAD at build time), `profile`, `exe`, `exe_modified_utc` |
| `host` | app, CLI | `os`, `arch`, `cpus`, `name`; on Linux also `kernel`, `cpu`, `cpu_governor`, `mem_total_mb`, and the load at the start (`load_1m`, `load_5m`, `mem_available_mb`) |
| `command_line` | app, CLI | the program's arguments |
| `output` | app, CLI | `folder` and its file system (`fs`, Linux: `ntfs3`, `fuseblk`, `btrfs`...) |
| `app` | app | `picture_mode` and every video setting (`video`) at the start; per receiver its settings, description, C5 `ver` reply and whether the host runs the gain loop; the open views |
| `start` | app | each receiver's state when recording started: decoder statistics (`stats`), USB stream counters (`stream`), lines the DSP thread dropped, gain |
| `summary` | app | written at close: `ended_utc`, `duration_s`, `bytes`, `rec_queue_dropped_blocks` (disk too slow), `fifo_full_polls` and `fifo_lost_ms_est`, the picture mode and video settings at the end (`end`), views, host load, each receiver's state |

## Events (`NAME.events.jsonl`)

Every event has `rx` (or null for session-wide), `host_ns`, `kind`, and
`byte` (offset in that receiver's .bin when the event was observed; for
commands this is the stream position at the time the command was sent,
since the exact sample a C5 command takes effect is found later from the
data).

| kind | extra fields |
|---|---|
| `gain` | `index` |
| `tune` | `mhz`, `offset_khz` |
| `clk` | `divider` |
| `slip` | `count` |
| `probe` | `score`, `verdict` (`clean`/`mixed`) |
| `fx2_status` | the decoded status block |
| `usb_gap` | `lost_bytes` (estimated), `reason` |
| `transfer` | `bytes` (every 64th bulk completion: timing for latency analysis) |
| `marker` | `text` (user marker key) |
| `c5` | `line` (raw console line) |
| `fifo_full` | `polls` (the FX2 found its FIFO full this many times since its last report, about 0.83 us of samples each) |
| `video_settings` | `picture_mode`, `video` (every video setting), `file_bytes` (each receiver's file size then, in rx order); app, on every change |
| `rx_stats` | `stats`, `stream`, `dsp_lines_dropped`, `gain`, `connected` (as in `context.start`); app, once a second per receiver, `byte` = that receiver's file size then |
| `app_stats` | `host` (load, free memory), `rec_queue_dropped`, `views` (lost lines, pipeline rebuilds, filter state); app, once a second |

Floating-point statistics that have no value yet (NaN) are written as null.

## Rolling buffer

The live app keeps the last 30 s (configurable) of raw blocks in RAM per
receiver. Pressing record writes the buffer first, then continues live, so
a recording can start before the button was pressed.

## Size

13.33 MS/s x 2 bytes = 26.7 MB/s = 1.6 GB per minute per receiver.
