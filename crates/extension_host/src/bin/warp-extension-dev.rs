//! A development host for Warp extensions.
//!
//! Stands in for Warp so a plugin can be exercised before the app-side host
//! exists, and afterwards as the fastest way to see what a plugin actually
//! sends. It drives the real [`Session`], so the permission, capability and
//! handshake gates here are the ones Warp enforces — a call this host refuses
//! is a call Warp would refuse.
//!
//! ```text
//! cargo run -p extension_host --bin warp-extension-dev -- \
//!     --extension-dev-path crates/extension_git --repo /path/to/repo
//! ```
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use command::blocking::Command;
use extension_host::{
    DiscoveredExtension, Dispatched, ExtensionHost, ExtensionProcess, ExtensionRecord,
    ProcessEvent, Session, discover_in,
};
use extension_protocol::{
    ActionOrigin, Capability, CommandInvokedParams, EventEnvelope, EventKind, ExecutionRunParams,
    ExecutionRunResult, ExecutionTarget, ExtensionError, ExtensionManifest, Message, Method,
    PanelActionParams, PanelItem, PanelStatus, PanelViewState, WorkspaceContext,
};

const POLL_INTERVAL: Duration = Duration::from_millis(40);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

const DIM: &str = "\x1b[90m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

struct Options {
    extension_path: PathBuf,
    repository: Option<PathBuf>,
    cwd: PathBuf,
    target: ExecutionTarget,
    trace: bool,
}

fn main() {
    let options = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{RED}{message}{RESET}\n");
            eprintln!("{}", usage());
            std::process::exit(2);
        }
    };
    if let Err(message) = run(options) {
        eprintln!("{RED}{message}{RESET}");
        std::process::exit(1);
    }
}

fn usage() -> String {
    "\
Usage: warp-extension-dev --extension-dev-path <dir> [options]

  --extension-dev-path <dir>  Extension directory holding extension.toml
  --repo <dir>                Repository root reported to the plugin
  --cwd <dir>                 Working directory reported (default: --repo, else $PWD)
  --target local|ssh:<id>     Execution target reported (default: local)
  --trace                     Print every protocol frame

Once running, type:
  r                      refresh (sends repository.changed)
  c <command-id>         invoke a declared command
  a <section> <item> <action>   activate a panel row action
  p                      reprint the last panel
  q                      quit"
        .to_owned()
}

fn parse_args() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mut extension_path = None;
    let mut repository = None;
    let mut cwd = None;
    let mut target = ExecutionTarget::Local;
    let mut trace = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--extension-dev-path" => {
                extension_path = Some(PathBuf::from(
                    args.next()
                        .ok_or("--extension-dev-path needs a directory")?,
                ));
            }
            "--repo" => {
                repository = Some(PathBuf::from(
                    args.next().ok_or("--repo needs a directory")?,
                ));
            }
            "--cwd" => cwd = Some(PathBuf::from(args.next().ok_or("--cwd needs a directory")?)),
            "--target" => {
                let value = args.next().ok_or("--target needs a value")?;
                target = match value.split_once(':') {
                    Some(("ssh", session_id)) => ExecutionTarget::Ssh {
                        session_id: session_id.to_owned(),
                    },
                    _ if value == "local" => ExecutionTarget::Local,
                    _ => return Err(format!("unrecognised target `{value}`")),
                };
            }
            "--trace" => trace = true,
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("unrecognised argument `{other}`")),
        }
    }

    let extension_path = extension_path.ok_or("--extension-dev-path is required")?;
    let cwd = cwd
        .or_else(|| repository.clone())
        .or_else(|| std::env::current_dir().ok())
        .ok_or("could not determine a working directory")?;
    Ok(Options {
        extension_path,
        repository,
        cwd,
        target,
        trace,
    })
}

fn run(options: Options) -> Result<(), String> {
    let (directory, manifest, executable) = load(&options.extension_path)?;
    prompt_for_permissions(&manifest)?;

    println!(
        "{DIM}starting {} from {}{RESET}",
        manifest.id,
        executable.display()
    );
    let mut process = ExtensionProcess::spawn(&manifest.id, &executable, &directory, None)
        .map_err(|err| err.to_string())?;

    let input = spawn_stdin_reader();
    let mut host = DevHost {
        options_cwd: options.cwd.clone(),
        repository: options.repository.clone(),
        target: options.target.clone(),
        trace: options.trace,
        input: Rc::clone(&input),
        last_panel: Rc::new(RefCell::new(None)),
    };
    let last_panel = Rc::clone(&host.last_panel);
    let mut session = Session::new(manifest.clone());

    println!("{DIM}type `q` to quit, or `--help` output for the rest{RESET}\n");

    loop {
        match process.recv_timeout(POLL_INTERVAL) {
            Ok(ProcessEvent::Message(message)) => {
                if options.trace {
                    trace_incoming(&message);
                }
                if let Some(reply) = session.handle_message(*message, &mut host) {
                    process.send(&reply).map_err(|err| err.to_string())?;
                }
                continue;
            }
            Ok(ProcessEvent::Decode(error)) => {
                println!("{RED}the plugin sent an unusable frame: {error}{RESET}");
                continue;
            }
            Ok(ProcessEvent::Stderr(line)) => {
                println!("{DIM}plugin | {line}{RESET}");
                continue;
            }
            Ok(ProcessEvent::Closed) => {
                println!("\n{YELLOW}the plugin closed its stream{RESET}");
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        let Ok(line) = input.try_recv() else { continue };
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        let (verb, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        match verb {
            "q" | "quit" => break,
            "p" => match last_panel.borrow().as_ref() {
                Some(panel) => print_panel(panel),
                None => println!("{DIM}no panel published yet{RESET}"),
            },
            "r" => send_event(
                &mut process,
                EventEnvelope::with_params(
                    EventKind::RepositoryChanged,
                    &serde_json::json!({ "workspace_id": "dev" }),
                )
                .map_err(|err| err.to_string())?,
                &options,
            )?,
            "c" => {
                if !manifest.declares_command(rest) {
                    println!("{RED}`{rest}` is not a command this manifest declares{RESET}");
                    continue;
                }
                send_event(
                    &mut process,
                    EventEnvelope::with_params(
                        EventKind::CommandInvoked,
                        &CommandInvokedParams {
                            command_id: rest.to_owned(),
                            origin: origin(&options),
                        },
                    )
                    .map_err(|err| err.to_string())?,
                    &options,
                )?;
            }
            "a" => {
                let parts: Vec<&str> = rest.splitn(3, ' ').collect();
                let [section, item, action] = parts.as_slice() else {
                    println!("{RED}usage: a <section> <item> <action>{RESET}");
                    continue;
                };
                let panel_id = last_panel
                    .borrow()
                    .as_ref()
                    .map(|panel| panel.panel_id.clone())
                    .unwrap_or_default();
                send_event(
                    &mut process,
                    EventEnvelope::with_params(
                        EventKind::PanelAction,
                        &PanelActionParams {
                            panel_id,
                            section_id: (*section).to_owned(),
                            item_id: (*item).to_owned(),
                            action_id: (*action).to_owned(),
                            origin: origin(&options),
                        },
                    )
                    .map_err(|err| err.to_string())?,
                    &options,
                )?;
            }
            other => println!("{RED}unrecognised input `{other}`{RESET}"),
        }
    }

    let status = process.stop(STOP_TIMEOUT);
    println!("{DIM}plugin exited: {status:?}{RESET}");
    Ok(())
}

/// Validates the extension the way Warp does, so a manifest error shows up here
/// with the same reason it would show up there.
fn load(path: &Path) -> Result<(PathBuf, ExtensionManifest, PathBuf), String> {
    let directory = path
        .canonicalize()
        .map_err(|err| format!("{}: {err}", path.display()))?;
    let parent = directory
        .parent()
        .ok_or("the extension directory has no parent")?;

    let discovered: Vec<DiscoveredExtension> = discover_in(parent)
        .into_iter()
        .filter(|extension| extension.directory == directory)
        .collect();
    let Some(extension) = discovered.into_iter().next() else {
        return Err(format!(
            "no extension.toml in {}. Is this an extension directory?",
            directory.display()
        ));
    };

    match extension.record {
        ExtensionRecord::Valid {
            manifest,
            executable,
        } => Ok((directory, *manifest, executable)),
        ExtensionRecord::Invalid { reason } => Err(format!("the manifest is not usable: {reason}")),
    }
}

/// The permission prompt, in the terminal.
///
/// The point is not the presentation but the gate: declining here leaves the
/// plugin unstarted, exactly as declining in Warp would.
fn prompt_for_permissions(manifest: &ExtensionManifest) -> Result<(), String> {
    println!("\n{BOLD}{} wants permission to:{RESET}", manifest.name);
    for permission in manifest.effective_permissions().iter() {
        println!("  {GREEN}✓{RESET} {}", permission.description());
    }
    if !manifest.execution.allowed_executables.is_empty() {
        println!(
            "  {GREEN}✓{RESET} Run only: {}",
            manifest.execution.allowed_executables.join(", ")
        );
    }
    print!("\n[a]llow / [c]ancel? ");
    use std::io::Write as _;
    std::io::stdout().flush().ok();

    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|err| err.to_string())?;
    if answer.trim().eq_ignore_ascii_case("a") {
        println!();
        Ok(())
    } else {
        Err("cancelled; the extension was not started".to_owned())
    }
}

fn spawn_stdin_reader() -> Rc<Receiver<String>> {
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                return;
            }
        }
    });
    Rc::new(receiver)
}

fn origin(options: &Options) -> ActionOrigin {
    ActionOrigin {
        workspace_id: "dev".to_owned(),
        session_id: match &options.target {
            ExecutionTarget::Local => None,
            ExecutionTarget::Ssh { session_id } => Some(session_id.clone()),
        },
        repository_root: options
            .repository
            .as_ref()
            .map(|path| path.display().to_string()),
        execution_target: options.target.clone(),
    }
}

fn send_event(
    process: &mut ExtensionProcess,
    event: EventEnvelope,
    options: &Options,
) -> Result<(), String> {
    if options.trace {
        println!("{CYAN}<- warp   event {}{RESET}", event.event);
    }
    process
        .send(&Message::Event(event))
        .map_err(|err| err.to_string())
}

fn trace_incoming(message: &Message) {
    match message {
        Message::Request(request) => println!(
            "{GREEN}-> plugin {} (#{}){RESET} {DIM}{}{RESET}",
            request.method, request.request_id, request.params
        ),
        Message::Event(event) => println!("{GREEN}-> plugin event {}{RESET}", event.event),
        Message::Response(response) => {
            println!("{GREEN}-> plugin response #{}{RESET}", response.request_id)
        }
    }
}

/// Answers the plugin's calls the way Warp would, and shows what it asked for.
struct DevHost {
    options_cwd: PathBuf,
    repository: Option<PathBuf>,
    target: ExecutionTarget,
    trace: bool,
    input: Rc<Receiver<String>>,
    last_panel: Rc<RefCell<Option<PanelViewState>>>,
}

impl DevHost {
    /// Blocks for a line of terminal input, which is what a dialog is.
    fn ask(&self, prompt: &str) -> String {
        use std::io::Write as _;
        print!("{YELLOW}{prompt}{RESET} ");
        std::io::stdout().flush().ok();
        self.input.recv().unwrap_or_default().trim().to_owned()
    }

    /// Runs the requested program for real.
    ///
    /// The allowlist is not re-checked here: `Session` already refused anything
    /// the manifest did not declare before this was reached.
    fn execute(&self, params: &ExecutionRunParams) -> ExecutionRunResult {
        let started = instant::Instant::now();
        let output = Command::new(&params.executable)
            .current_dir(&params.cwd)
            .args(&params.args)
            .envs(params.env.clone())
            .output();
        let duration_ms = started.elapsed().as_millis() as u64;

        match output {
            Ok(output) => ExecutionRunResult {
                exit_code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                duration_ms,
                truncated: false,
            },
            Err(err) => ExecutionRunResult {
                exit_code: None,
                stdout: String::new(),
                stderr: err.to_string(),
                duration_ms,
                truncated: false,
            },
        }
    }
}

impl ExtensionHost for DevHost {
    fn capabilities(&self) -> Vec<Capability> {
        Capability::ALL.to_vec()
    }

    /// Every method is answered inline: the terminal prompts block, so there is
    /// nothing here that has to be deferred the way a Warp modal does.
    fn dispatch(
        &mut self,
        _request_id: &str,
        method: Method,
        params: serde_json::Value,
    ) -> Dispatched {
        Dispatched::Answered(self.answer(method, params))
    }
}

impl DevHost {
    fn answer(
        &mut self,
        method: Method,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ExtensionError> {
        match method {
            Method::WorkspaceGetContext => Ok(serde_json::to_value(WorkspaceContext {
                workspace_id: "dev".to_owned(),
                cwd: self.options_cwd.display().to_string(),
                repository_root: self
                    .repository
                    .as_ref()
                    .map(|path| path.display().to_string()),
                active_pane_id: Some("dev-pane".to_owned()),
                session_id: match &self.target {
                    ExecutionTarget::Local => None,
                    ExecutionTarget::Ssh { session_id } => Some(session_id.clone()),
                },
                execution_target: self.target.clone(),
            })
            .unwrap_or_default()),
            Method::ExecutionRun => {
                let params: ExecutionRunParams =
                    extension_protocol::decode_params(method, &params)?;
                let result = self.execute(&params);
                if self.trace {
                    println!(
                        "{DIM}   ran {} {} -> {:?} in {}ms{RESET}",
                        params.executable,
                        params.args.join(" "),
                        result.exit_code,
                        result.duration_ms
                    );
                }
                Ok(serde_json::to_value(result).unwrap_or_default())
            }
            Method::PanelSetState => {
                let state: PanelViewState = extension_protocol::decode_params(method, &params)?;
                // Loading frames are noise here: the plugin publishes one
                // before every refresh and replaces it within milliseconds.
                if !matches!(state.status, PanelStatus::Loading) {
                    print_panel(&state);
                }
                *self.last_panel.borrow_mut() = Some(state);
                Ok(serde_json::json!({}))
            }
            Method::NotificationShow => {
                let title = params["title"].as_str().unwrap_or_default();
                let body = params["body"].as_str().unwrap_or_default();
                println!("{YELLOW}▸ {title}{RESET}");
                if !body.is_empty() {
                    println!("{DIM}  {body}{RESET}");
                }
                Ok(serde_json::json!({}))
            }
            Method::DialogConfirm => {
                let title = params["title"].as_str().unwrap_or_default();
                if let Some(body) = params["body"].as_str() {
                    println!("{DIM}  {body}{RESET}");
                }
                let destructive = params["destructive"].as_bool().unwrap_or(false);
                let marker = if destructive { RED } else { CYAN };
                let answer = self.ask(&format!("{marker}? {title}{RESET} [y/N]"));
                Ok(serde_json::json!({
                    "confirmed": answer.eq_ignore_ascii_case("y")
                }))
            }
            Method::DialogInput => {
                let title = params["title"].as_str().unwrap_or_default();
                let answer = self.ask(&format!("? {title}:"));
                Ok(if answer.is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::json!({ "value": answer })
                })
            }
            Method::DialogSelect => {
                let title = params["title"].as_str().unwrap_or_default();
                let options: Vec<String> = params["options"]
                    .as_array()
                    .map(|options| {
                        options
                            .iter()
                            .filter_map(|option| option["id"].as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                println!("{CYAN}? {title}{RESET}");
                for (index, option) in options.iter().enumerate() {
                    println!("  {}) {option}", index + 1);
                }
                let answer = self.ask("choose a number (blank cancels):");
                let chosen = answer
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| options.get(index.wrapping_sub(1)));
                Ok(match chosen {
                    Some(id) => serde_json::json!({ "selected_id": id }),
                    None => serde_json::json!({}),
                })
            }
            Method::FileOpen => {
                println!(
                    "{CYAN}▸ would open file{RESET} {}",
                    params["path"].as_str().unwrap_or_default()
                );
                Ok(serde_json::json!({}))
            }
            Method::DiffOpenFile => {
                println!(
                    "{CYAN}▸ would open diff{RESET} {} {DIM}({}){RESET}",
                    params["path"].as_str().unwrap_or_default(),
                    params["comparison"].as_str().unwrap_or_default()
                );
                Ok(serde_json::json!({}))
            }
            Method::DiffOpenWorkingTree => {
                println!(
                    "{CYAN}▸ would open working-tree diff{RESET} {}",
                    params["repository_root"].as_str().unwrap_or_default()
                );
                Ok(serde_json::json!({}))
            }
            Method::ExtensionInitialize => Ok(serde_json::json!({})),
        }
    }
}

fn print_panel(state: &PanelViewState) {
    println!(
        "\n{BOLD}┌─ {} {RESET}{DIM}({}){RESET}",
        state.panel_id,
        describe_status(&state.status)
    );
    for section in &state.sections {
        let badge = section
            .badge
            .as_ref()
            .map(|badge| format!(" {DIM}({badge}){RESET}"))
            .unwrap_or_default();
        println!(
            "{BOLD}│ {}{RESET}{badge}  {DIM}{}{RESET}",
            section.title, section.id
        );
        for item in &section.items {
            print_item(item, 1);
        }
    }
    println!("{BOLD}└─{RESET}\n");
}

fn print_item(item: &PanelItem, depth: usize) {
    let indent = "  ".repeat(depth);
    let badge = item
        .badge
        .as_ref()
        .map(|badge| format!(" [{badge}]"))
        .unwrap_or_default();
    let actions = if item.actions.is_empty() {
        String::new()
    } else {
        let mut names: VecDeque<&str> = item
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        let mut rendered = Vec::new();
        while let Some(name) = names.pop_front() {
            rendered.push(name.to_owned());
        }
        format!("  {DIM}<{}>{RESET}", rendered.join(" "))
    };
    println!("│{indent}{}{badge}{actions}", item.label);
    if let Some(description) = &item.description {
        println!("│{indent}  {DIM}{description}{RESET}");
    }
    for child in &item.children {
        print_item(child, depth + 1);
    }
}

fn describe_status(status: &PanelStatus) -> String {
    match status {
        PanelStatus::Loading => "loading".to_owned(),
        PanelStatus::Ready => "ready".to_owned(),
        PanelStatus::Empty { message } => format!("empty: {message}"),
        PanelStatus::Error { message } => format!("error: {message}"),
    }
}
