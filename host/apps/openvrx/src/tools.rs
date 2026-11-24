// SPDX-License-Identifier: GPL-3.0-or-later
//! External tools for recordings: programs the recordings page can run on a
//! recording (render a video, restore one, export, anything that takes a
//! file and writes a file). The app knows nothing about what a tool does;
//! each one is a JSON entry in `<settings folder>/tools.d/*.json`:
//!
//! ```json
//! { "tools": [ {
//!     "name": "Render video",
//!     "description": "What it does, one line",
//!     "program": "~/bin/my-renderer",
//!     "args": ["{input}", "{output}", "--crf", "18"],
//!     "input": "raw",
//!     "output_suffix": "-render.mp4"
//! } ] }
//! ```
//!
//! `input` is `raw` (a raw recording; `{input}` is its `NAME.openvrx.json`)
//! or `video` (an MP4, MKV, WebM, MOV or AVI file). Placeholders in `args`:
//! `{input}`, `{output}` (`{base}` + `output_suffix`, next to the input, with
//! `-2`, `-3`... added rather than overwriting a file), `{base}` (the input
//! without its extension), `{dir}`. `program` may start with `~/`; a
//! relative path with a slash is taken from the JSON file's real folder
//! (symlinks followed), so a tools file can sit beside its programs.
//! `low_priority` (default true) runs the tool under `nice -n 10` (Windows:
//! below-normal priority) so it does not take CPU from the live picture.
//! A file may also be a bare array of tools.

use serde::Deserialize;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Output lines kept per job.
const LOG_LINES: usize = 300;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    #[default]
    Raw,
    Video,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub input: InputKind,
    #[serde(default)]
    pub output_suffix: Option<String>,
    #[serde(default = "yes")]
    pub low_priority: bool,
    /// Folder of the JSON file it came from (relative programs resolve here).
    #[serde(skip)]
    pub origin: PathBuf,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ToolsFile {
    Wrapped { tools: Vec<ToolSpec> },
    List(Vec<ToolSpec>),
}

/// Parses one tools file (`origin`: the folder its relative programs are in).
pub fn parse_tools(text: &str, origin: &Path) -> Result<Vec<ToolSpec>, String> {
    let f: ToolsFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut list = match f {
        ToolsFile::Wrapped { tools } | ToolsFile::List(tools) => tools,
    };
    for t in &mut list {
        t.origin = origin.to_path_buf();
        if t.name.trim().is_empty() || t.program.trim().is_empty() {
            return Err("every tool needs a name and a program".into());
        }
    }
    Ok(list)
}

/// Every tool in `dir/*.json`, sorted by name, and a note per file that
/// could not be read. A missing folder is no tools, not an error.
pub fn load_tools(dir: &Path) -> (Vec<ToolSpec>, Vec<String>) {
    let mut tools = Vec::new();
    let mut errors = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return (tools, errors);
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("json")))
        .collect();
    files.sort();
    for f in files {
        let real = std::fs::canonicalize(&f).unwrap_or_else(|_| f.clone());
        let origin = real.parent().map(Path::to_path_buf).unwrap_or_default();
        match std::fs::read_to_string(&real).map_err(|e| e.to_string()).and_then(|t| parse_tools(&t, &origin)) {
            Ok(list) => tools.extend(list),
            Err(e) => errors.push(format!("{}: {e}", f.display())),
        }
    }
    tools.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    (tools, errors)
}

/// The tools folder beside the settings file.
pub fn tools_dir(settings_path: &Path) -> PathBuf {
    settings_path.parent().map(|d| d.join("tools.d")).unwrap_or_else(|| PathBuf::from("tools.d"))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

/// `~/x` to the home folder; a relative path with a separator to `origin`.
fn resolve_program(program: &str, origin: &Path) -> PathBuf {
    if let Some(rest) = program.strip_prefix("~/").or_else(|| program.strip_prefix("~\\")) {
        if let Some(h) = home() {
            return h.join(rest);
        }
    }
    let p = PathBuf::from(program);
    if p.is_relative() && (program.contains('/') || program.contains('\\')) {
        return origin.join(p);
    }
    p
}

/// The input without its extension; a raw recording's `NAME.openvrx.json`
/// loses both parts.
pub fn base_of(input: &Path) -> PathBuf {
    let s = input.to_string_lossy();
    for ext in [".openvrx.json", ".events.jsonl"] {
        if let Some(b) = s.strip_suffix(ext) {
            return PathBuf::from(b);
        }
    }
    input.with_extension("")
}

/// `base + suffix`, or with `-2`, `-3`... before the extension when that
/// file exists (`exists` is the file system check, a parameter for tests).
pub fn free_output(base: &Path, suffix: &str, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let first = PathBuf::from(format!("{}{suffix}", base.display()));
    if !exists(&first) {
        return first;
    }
    let (stem, ext) = match suffix.rfind('.') {
        Some(k) => (&suffix[..k], &suffix[k..]),
        None => (suffix, ""),
    };
    (2..)
        .map(|n| PathBuf::from(format!("{}{stem}-{n}{ext}", base.display())))
        .find(|p| !exists(p))
        .unwrap_or(first)
}

/// The tool's argument list with the placeholders filled in.
pub fn expand_args(args: &[String], input: &Path, output: Option<&Path>) -> Vec<String> {
    let base = base_of(input);
    let dir = input.parent().map(Path::to_path_buf).unwrap_or_default();
    args.iter()
        .map(|a| {
            a.replace("{input}", &input.display().to_string())
                .replace("{output}", &output.map(|o| o.display().to_string()).unwrap_or_default())
                .replace("{base}", &base.display().to_string())
                .replace("{dir}", &dir.display().to_string())
        })
        .collect()
}

/// The last percentage in a line ("frame 120 (42.5 %)", "42%"), 0..1.
pub fn parse_percent(line: &str) -> Option<f32> {
    let b = line.as_bytes();
    let mut found = None;
    for (k, &c) in b.iter().enumerate() {
        if c != b'%' {
            continue;
        }
        let mut end = k;
        while end > 0 && b[end - 1] == b' ' {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && (b[start - 1].is_ascii_digit() || b[start - 1] == b'.') {
            start -= 1;
        }
        if start < end {
            if let Ok(v) = line[start..end].parse::<f32>() {
                if (0.0..=100.0).contains(&v) {
                    found = Some(v / 100.0);
                }
            }
        }
    }
    found
}

#[derive(Clone, Debug, PartialEq)]
pub enum JobState {
    Running,
    Done,
    Failed(String),
    Cancelled,
}

/// One run of a tool.
pub struct Job {
    pub id: u64,
    pub tool: String,
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub started: Instant,
    pub ended: Option<Instant>,
    pub state: JobState,
    log: Arc<Mutex<VecDeque<String>>>,
    child: Option<Child>,
    cancel_requested: bool,
}

impl Job {
    pub fn log_lines(&self) -> Vec<String> {
        self.log.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default()
    }

    pub fn last_line(&self) -> Option<String> {
        self.log.lock().ok().and_then(|l| l.back().cloned())
    }

    /// Progress from the newest line that holds a percentage.
    pub fn progress(&self) -> Option<f32> {
        let l = self.log.lock().ok()?;
        l.iter().rev().take(8).find_map(|s| parse_percent(s))
    }

    pub fn elapsed(&self) -> Duration {
        self.ended.unwrap_or_else(Instant::now) - self.started
    }

    pub fn running(&self) -> bool {
        self.state == JobState::Running
    }
}

/// Every job of this session, newest last.
#[derive(Default)]
pub struct Jobs {
    pub list: Vec<Job>,
    next_id: u64,
}

impl Jobs {
    /// Starts `tool` on `input`. Err: the program could not be started.
    pub fn start(&mut self, tool: &ToolSpec, input: &Path) -> Result<u64, String> {
        let output = tool
            .output_suffix
            .as_deref()
            .map(|s| free_output(&base_of(input), s, |p| p.exists()));
        let program = resolve_program(&tool.program, &tool.origin);
        let args = expand_args(&tool.args, input, output.as_deref());
        log::info!("tool {}: {} {:?}", tool.name, program.display(), args);
        let child = spawn(&program, &args, tool.low_priority, input.parent())
            .map_err(|e| format!("cannot start {}: {e}", program.display()))?;
        self.next_id += 1;
        let mut job = Job {
            id: self.next_id,
            tool: tool.name.clone(),
            input: input.to_path_buf(),
            output,
            started: Instant::now(),
            ended: None,
            state: JobState::Running,
            log: Arc::new(Mutex::new(VecDeque::new())),
            child: Some(child),
            cancel_requested: false,
        };
        if let Some(c) = job.child.as_mut() {
            if let Some(out) = c.stdout.take() {
                pump(out, Arc::clone(&job.log));
            }
            if let Some(err) = c.stderr.take() {
                pump(err, Arc::clone(&job.log));
            }
        }
        self.list.push(job);
        Ok(self.next_id)
    }

    /// Updates finished jobs; returns the outputs of jobs that just succeeded.
    pub fn poll(&mut self) -> Vec<PathBuf> {
        let mut finished = Vec::new();
        for j in &mut self.list {
            let Some(c) = j.child.as_mut() else { continue };
            match c.try_wait() {
                Ok(None) => {}
                Ok(Some(status)) => {
                    j.child = None;
                    j.ended = Some(Instant::now());
                    j.state = if j.cancel_requested {
                        JobState::Cancelled
                    } else if status.success() {
                        if let Some(o) = &j.output {
                            finished.push(o.clone());
                        }
                        JobState::Done
                    } else {
                        JobState::Failed(match status.code() {
                            Some(c) => format!("exit code {c}"),
                            None => "stopped by a signal".into(),
                        })
                    };
                }
                Err(e) => {
                    j.child = None;
                    j.ended = Some(Instant::now());
                    j.state = JobState::Failed(e.to_string());
                }
            }
        }
        finished
    }

    pub fn cancel(&mut self, id: u64) {
        if let Some(j) = self.list.iter_mut().find(|j| j.id == id) {
            if let Some(c) = j.child.as_mut() {
                j.cancel_requested = true;
                let _ = c.kill();
            }
        }
    }

    /// Removes finished jobs from the list.
    pub fn clear_finished(&mut self) {
        self.list.retain(Job::running);
    }

    pub fn running(&self) -> usize {
        self.list.iter().filter(|j| j.running()).count()
    }
}

impl Drop for Jobs {
    /// Closing the app stops its tools (a half-written output is left as is).
    fn drop(&mut self) {
        for j in &mut self.list {
            if let Some(c) = j.child.as_mut() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
}

fn spawn(program: &Path, args: &[String], low: bool, cwd: Option<&Path>) -> std::io::Result<Child> {
    let build = |via_nice: bool| {
        let mut cmd = if via_nice {
            let mut c = Command::new("nice");
            c.arg("-n").arg("10").arg(program);
            c
        } else {
            Command::new(program)
        };
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(d) = cwd {
            cmd.current_dir(d);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW, plus BELOW_NORMAL_PRIORITY_CLASS when asked.
            cmd.creation_flags(0x0800_0000 | if low { 0x0000_4000 } else { 0 });
        }
        cmd
    };
    if cfg!(unix) && low {
        // `nice` replaces itself with the program (exec), so killing the
        // child kills the tool itself. Without `nice`, run it plainly. A
        // missing program then shows as a failed job with nice's message.
        if let Ok(c) = build(true).spawn() {
            return Ok(c);
        }
    }
    build(false).spawn()
}

/// Reads a pipe into the log, splitting on `\n` and `\r` (progress bars
/// rewrite their line with `\r`).
fn pump(pipe: impl Read + Send + 'static, log: Arc<Mutex<VecDeque<String>>>) {
    let _ = std::thread::Builder::new().name("tool-output".into()).spawn(move || {
        let mut r = BufReader::new(pipe);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let n = match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let text = String::from_utf8_lossy(&buf[..n]);
            let Ok(mut l) = log.lock() else { break };
            for part in text.split(['\r', '\n']).map(str::trim_end).filter(|s| !s.is_empty()) {
                l.push_back(part.to_string());
                while l.len() > LOG_LINES {
                    l.pop_front();
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent() {
        assert_eq!(parse_percent("no number here"), None);
        assert_eq!(parse_percent("42%"), Some(0.42));
        assert_eq!(parse_percent("frame 120 (42.5 %)"), Some(0.425));
        assert_eq!(parse_percent("10% then 80%"), Some(0.8));
        assert_eq!(parse_percent("250% boost"), None);
        assert_eq!(parse_percent("%"), None);
    }

    #[test]
    fn bases_and_outputs() {
        assert_eq!(base_of(Path::new("/r/openvrx-1.openvrx.json")), PathBuf::from("/r/openvrx-1"));
        assert_eq!(base_of(Path::new("/r/flight.mp4")), PathBuf::from("/r/flight"));
        let base = Path::new("/r/a");
        assert_eq!(free_output(base, "-clean.mp4", |_| false), PathBuf::from("/r/a-clean.mp4"));
        let taken = |p: &Path| p == Path::new("/r/a-clean.mp4") || p == Path::new("/r/a-clean-2.mp4");
        assert_eq!(free_output(base, "-clean.mp4", taken), PathBuf::from("/r/a-clean-3.mp4"));
    }

    #[test]
    fn args_expand() {
        let a = vec!["{input}".to_string(), "-o".into(), "{output}".into(), "{base}.log".into(), "{dir}".into()];
        let got = expand_args(&a, Path::new("/r/x.openvrx.json"), Some(Path::new("/r/x-v.mp4")));
        assert_eq!(got, ["/r/x.openvrx.json", "-o", "/r/x-v.mp4", "/r/x.log", "/r"]);
    }

    #[test]
    fn files_parse() {
        let o = Path::new("/t");
        let one = r#"{"tools":[{"name":"A","program":"bin/a","args":["{input}"],"input":"video","output_suffix":".x.mp4"}]}"#;
        let t = parse_tools(one, o).unwrap();
        assert_eq!(t[0].input, InputKind::Video);
        assert!(t[0].low_priority);
        assert_eq!(resolve_program(&t[0].program, &t[0].origin), PathBuf::from("/t/bin/a"));
        assert_eq!(resolve_program("ffmpeg", o), PathBuf::from("ffmpeg"));
        let list = r#"[{"name":"B","program":"b"}]"#;
        assert_eq!(parse_tools(list, o).unwrap()[0].input, InputKind::Raw);
        assert!(parse_tools(r#"[{"name":"","program":"b"}]"#, o).is_err());
    }
}
