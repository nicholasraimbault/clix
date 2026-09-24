use std::time::{Duration, SystemTime};

use chrono::{Local, NaiveDate, NaiveTime, TimeZone};
use clap::{ColorChoice, Parser, Subcommand};

use crate::error::{ClixError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    Add {
        tool: String,
        allow: Vec<String>,
        once: bool,
        for_dur: Option<Duration>,
        until: Option<SystemTime>,
        days: Vec<String>,
        dates: Vec<u8>,
        from: Option<String>,
        to: Option<String>,
        weekdays: bool,
    },
    Exec {
        body: String,
        argv: Vec<String>,
        no_wait: bool,
    },
    Pair {
        phrase: Option<String>,
        name: Option<String>,
    },
    Hands,
    Remove {
        tool: String,
    },
    Log,
    Job(JobCommand),
    Storage(StorageCommand),
    Pin(PinCommand),
    Pending,
    Allow {
        request_id: String,
        allow: Vec<String>,
        once: bool,
        for_dur: Option<Duration>,
        until: Option<SystemTime>,
        days: Vec<String>,
        dates: Vec<u8>,
        from: Option<String>,
        to: Option<String>,
        weekdays: bool,
    },
    Deny {
        request_id: String,
    },
    Request {
        body: String,
        tool: String,
    },
    Daemon,
    Install,
    Status,
}

#[derive(Parser, Debug)]
#[command(name = "clix", disable_help_subcommand = true, color = ColorChoice::Never)]
#[command(allow_external_subcommands = true)]
#[command(
    after_help = "Named machine: clix [--no-wait] -- BODY TOOL ARG…\nUse this form when BODY matches an owner command; tool arguments are preserved."
)]
struct Cli {
    #[arg(long = "no-wait", global = true)]
    no_wait: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Add {
        tool: Option<String>,
        #[command(flatten)]
        grant: GrantCli,
    },
    Pair {
        phrase: Option<String>,
        #[arg(long = "name", value_name = "BODY")]
        name: Option<String>,
    },
    Hands,
    Remove {
        tool: Option<String>,
    },
    Log,
    Job {
        #[command(subcommand)]
        command: JobCommand,
    },
    Storage {
        #[command(subcommand)]
        command: StorageCommand,
    },
    Pin {
        #[command(subcommand)]
        command: PinCommand,
    },
    Pending,
    Allow {
        #[arg(value_name = "REQUEST_ID")]
        request_id: String,
        #[command(flatten)]
        grant: GrantCli,
    },
    Deny {
        #[arg(value_name = "REQUEST_ID")]
        request_id: String,
    },
    Request {
        body: Option<String>,
        tool: Option<String>,
    },
    Daemon,
    Install,
    Status,
    #[command(external_subcommand)]
    External(Vec<String>),
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum JobCommand {
    Inspect {
        job_id: String,
    },
    Output {
        job_id: String,
        #[arg(long)]
        stderr: bool,
    },
    Retry {
        job_id: String,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum StorageCommand {
    Status,
    Prune {
        #[arg(long, default_value_t = 0)]
        keep_output_jobs: usize,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum PinCommand {
    /// Synchronize pins with the named machine without running a tool.
    Sync { body: String },
    /// Inspect current conflicts with the named machine.
    Conflicts { body: String },
    /// Adopt the inspected peer version on this machine, retaining displaced data.
    TakePeer {
        body: String,
        path: String,
        #[arg(long)]
        token: String,
    },
    Recovery {
        #[command(subcommand)]
        command: RecoveryCommand,
    },
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum RecoveryCommand {
    List {
        #[arg(long)]
        after: Option<String>,
    },
    Inspect {
        id: String,
    },
    /// Write the inspected version's bytes to stdout; errors exit nonzero.
    Export {
        id: String,
        #[arg(value_parser=["receipt","live","retained"])]
        version: String,
        #[arg(long)]
        token: String,
    },
    /// Resolve exactly the versions shown by inspection.
    Resolve {
        id: String,
        #[arg(value_parser=["keep-live","restore-retained"])]
        choice: String,
        #[arg(long)]
        token: String,
    },
    /// Permanently remove an explicitly inspected retained version or artifact.
    Discard {
        id: String,
        #[arg(long)]
        token: String,
    },
}

#[derive(clap::Args, Debug)]
struct GrantCli {
    #[arg(long = "allow", value_name = "BODY")]
    allow: Vec<String>,
    #[arg(long = "all")]
    all: bool,
    #[arg(long = "server")]
    server: bool,
    #[arg(long = "once")]
    once: bool,
    #[arg(long = "for", value_name = "DUR")]
    for_dur: Option<String>,
    #[arg(long = "until", value_name = "TIME")]
    until: Option<String>,
    #[arg(long = "days", value_name = "DAYS", value_delimiter = ',')]
    days: Vec<String>,
    #[arg(long = "dates", value_name = "DATES", value_delimiter = ',')]
    dates: Vec<String>,
    #[arg(long = "from", value_name = "TIME")]
    from: Option<String>,
    #[arg(long = "to", value_name = "TIME")]
    to: Option<String>,
    #[arg(long = "weekdays")]
    weekdays: bool,
}

pub fn parse_argv(argv: &[String]) -> Result<Cmd> {
    // A new owner command must not make an existing paired body unreachable.
    // The explicit destination form also leaves every tool argument untouched.
    let explicit = if argv.get(1).is_some_and(|s| s == "--") {
        Some((2, false))
    } else if argv.get(1).is_some_and(|s| s == "--no-wait")
        && argv.get(2).is_some_and(|s| s == "--")
    {
        Some((3, true))
    } else {
        None
    };
    if let Some((start, no_wait)) = explicit {
        let parts = &argv[start..];
        if parts.len() < 2 {
            return Err(ClixError::Usage(
                "usage: clix [--no-wait] -- <body> <cmd>…".into(),
            ));
        }
        crate::pair::validate_name(&parts[0])?;
        return Ok(Cmd::Exec {
            body: parts[0].clone(),
            argv: parts[1..].to_vec(),
            no_wait,
        });
    }
    let cli = Cli::try_parse_from(argv).map_err(|e| ClixError::Usage(e.to_string()))?;
    match cli.command {
        None => Err(ClixError::Usage("usage: clix <command>".into())),
        Some(Commands::Add { tool, grant }) => {
            let tool = tool.ok_or_else(|| ClixError::Usage("usage: clix add <tool>".into()))?;
            let g = parse_grant_cli(grant, false, true)?;
            Ok(Cmd::Add {
                tool,
                allow: g.allow,
                once: g.once,
                for_dur: g.for_dur,
                until: g.until,
                days: g.days,
                dates: g.dates,
                from: g.from,
                to: g.to,
                weekdays: g.weekdays,
            })
        }
        Some(Commands::Pair { phrase, name }) => Ok(Cmd::Pair { phrase, name }),
        Some(Commands::Hands) => Ok(Cmd::Hands),
        Some(Commands::Remove { tool }) => {
            let tool = tool.ok_or_else(|| ClixError::Usage("usage: clix remove <tool>".into()))?;
            Ok(Cmd::Remove { tool })
        }
        Some(Commands::Log) => Ok(Cmd::Log),
        Some(Commands::Job { command }) => Ok(Cmd::Job(command)),
        Some(Commands::Storage { command }) => Ok(Cmd::Storage(command)),
        Some(Commands::Pin { command }) => Ok(Cmd::Pin(command)),
        Some(Commands::Pending) => Ok(Cmd::Pending),
        Some(Commands::Allow { request_id, grant }) => {
            let g = parse_grant_cli(grant, true, false)?;
            Ok(Cmd::Allow {
                request_id,
                allow: g.allow,
                once: g.once,
                for_dur: g.for_dur,
                until: g.until,
                days: g.days,
                dates: g.dates,
                from: g.from,
                to: g.to,
                weekdays: g.weekdays,
            })
        }
        Some(Commands::Deny { request_id }) => Ok(Cmd::Deny { request_id }),
        Some(Commands::Request { body, tool }) => {
            let body =
                body.ok_or_else(|| ClixError::Usage("usage: clix request <body> <tool>".into()))?;
            let tool =
                tool.ok_or_else(|| ClixError::Usage("usage: clix request <body> <tool>".into()))?;
            Ok(Cmd::Request { body, tool })
        }
        Some(Commands::Daemon) => Ok(Cmd::Daemon),
        Some(Commands::Install) => Ok(Cmd::Install),
        Some(Commands::Status) => Ok(Cmd::Status),
        Some(Commands::External(parts)) => {
            let (parts, no_wait) = strip_no_wait(parts, cli.no_wait);
            if parts.is_empty() {
                return Err(ClixError::Usage("usage: clix <body> <cmd>…".into()));
            }
            if parts.len() < 2 {
                return Err(ClixError::Usage("usage: clix <body> <cmd>…".into()));
            }
            let body = parts[0].clone();
            let argv = parts[1..].to_vec();
            Ok(Cmd::Exec {
                body,
                argv,
                no_wait,
            })
        }
    }
}

struct GrantNarrow {
    allow: Vec<String>,
    once: bool,
    for_dur: Option<Duration>,
    until: Option<SystemTime>,
    days: Vec<String>,
    dates: Vec<u8>,
    from: Option<String>,
    to: Option<String>,
    weekdays: bool,
}

fn parse_grant_cli(
    grant: GrantCli,
    default_once: bool,
    require_scope: bool,
) -> Result<GrantNarrow> {
    let GrantCli {
        mut allow,
        all,
        server,
        once,
        for_dur,
        until,
        days,
        dates,
        from,
        to,
        weekdays,
    } = grant;
    if all && (server || !allow.is_empty()) {
        return Err(ClixError::Usage(
            "use --all or --allow/--server, not both".into(),
        ));
    }
    // A grant must name its machines. Empty scope no longer defaults to every
    // paired machine (including ones paired later); require an explicit --all.
    if require_scope && !all && !server && allow.is_empty() {
        return Err(ClixError::Usage(
            "specify --allow <machine> (repeatable), --server, or --all".into(),
        ));
    }
    if server {
        allow.push("server".into());
    }
    if once && (!days.is_empty() || !dates.is_empty() || weekdays) {
        return Err(ClixError::Usage(
            "use --once or a schedule, not both".into(),
        ));
    }
    if weekdays && !days.is_empty() {
        return Err(ClixError::Usage(
            "use --days or --weekdays, not both".into(),
        ));
    }
    let widening = for_dur.is_some()
        || until.is_some()
        || !days.is_empty()
        || !dates.is_empty()
        || weekdays
        || from.is_some()
        || to.is_some();
    let once = once || (default_once && !widening);
    let days = validate_days(days)?;
    let dates = parse_dates(dates)?;
    if let Some(ref t) = from {
        validate_ampm(t)?;
    }
    if let Some(ref t) = to {
        validate_ampm(t)?;
    }
    let for_dur = for_dur.map(|s| parse_duration(&s)).transpose()?;
    let until = until.map(|s| parse_until(&s)).transpose()?;
    Ok(GrantNarrow {
        allow,
        once,
        for_dur,
        until,
        days,
        dates,
        from,
        to,
        weekdays,
    })
}

/// `--no-wait` before the body or between body and tool. Not stolen from tool argv.
fn strip_no_wait(mut parts: Vec<String>, mut no_wait: bool) -> (Vec<String>, bool) {
    if parts.first().map(String::as_str) == Some("--no-wait") {
        no_wait = true;
        parts.remove(0);
    }
    if parts.len() >= 2 && parts[1] == "--no-wait" {
        no_wait = true;
        parts.remove(1);
    }
    (parts, no_wait)
}

fn validate_days(days: Vec<String>) -> Result<Vec<String>> {
    const NAMES: &[&str] = &["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let mut out = Vec::with_capacity(days.len());
    for d in days {
        let lower = d.trim().to_ascii_lowercase();
        if !NAMES.contains(&lower.as_str()) {
            return Err(ClixError::Usage(format!(
                "unknown weekday '{d}' (use mon..sun)"
            )));
        }
        out.push(lower);
    }
    Ok(out)
}

fn parse_dates(dates: Vec<String>) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(dates.len());
    for d in dates {
        let raw = d.trim();
        let n: u8 = raw
            .parse()
            .map_err(|_| ClixError::Usage(format!("bad date '{d}' (use 1–31)")))?;
        if !(1..=31).contains(&n) {
            return Err(ClixError::Usage(format!("bad date '{d}' (use 1–31)")));
        }
        out.push(n);
    }
    Ok(out)
}

fn validate_ampm(s: &str) -> Result<()> {
    parse_ampm(s).map(|_| ())
}

pub(crate) fn parse_ampm_naive(s: &str) -> Result<NaiveTime> {
    let mins = parse_ampm(s)?;
    NaiveTime::from_hms_opt(mins / 60, mins % 60, 0)
        .ok_or_else(|| ClixError::Usage(format!("bad time '{s}' (use am/pm, e.g. 5pm)")))
}

/// Minutes since midnight from `9am` / `5pm`. Hours only; no 24-hour clock.
fn parse_ampm(s: &str) -> Result<u32> {
    let s = s.trim().to_ascii_lowercase();
    let (digits, pm) = if let Some(rest) = s.strip_suffix("pm") {
        (rest, true)
    } else if let Some(rest) = s.strip_suffix("am") {
        (rest, false)
    } else {
        return Err(ClixError::Usage(format!(
            "bad time '{s}' (use am/pm, e.g. 5pm)"
        )));
    };
    let hour: u32 = digits
        .parse()
        .map_err(|_| ClixError::Usage(format!("bad time '{s}' (use am/pm, e.g. 5pm)")))?;
    if !(1..=12).contains(&hour) {
        return Err(ClixError::Usage(format!(
            "bad time '{s}' (use am/pm, e.g. 5pm)"
        )));
    }
    let hour24 = match (hour, pm) {
        (12, false) => 0,
        (12, true) => 12,
        (h, false) => h,
        (h, true) => h + 12,
    };
    Ok(hour24 * 60)
}

fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim().to_ascii_lowercase();
    if s.len() < 2 {
        return Err(ClixError::Usage("bad duration (e.g. 2h, 30m)".into()));
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: u64 = num
        .parse()
        .map_err(|_| ClixError::Usage(format!("bad duration '{s}' (e.g. 2h, 30m)")))?;
    let secs = match unit {
        "s" => n,
        "m" => n
            .checked_mul(60)
            .ok_or_else(|| ClixError::Usage("duration too large".into()))?,
        "h" => n
            .checked_mul(3600)
            .ok_or_else(|| ClixError::Usage("duration too large".into()))?,
        "d" => n
            .checked_mul(86400)
            .ok_or_else(|| ClixError::Usage("duration too large".into()))?,
        _ => {
            return Err(ClixError::Usage(format!(
                "bad duration '{s}' (e.g. 2h, 30m)"
            )))
        }
    };
    Ok(Duration::from_secs(secs))
}

/// `--until 5pm` → local time today, or tomorrow if that time is already past.
fn parse_until(s: &str) -> Result<SystemTime> {
    let mins = parse_ampm(s)?;
    let hour = mins / 60;
    let minute = mins % 60;
    let naive = NaiveTime::from_hms_opt(hour, minute, 0)
        .ok_or_else(|| ClixError::Usage(format!("bad time '{s}' (use am/pm, e.g. 5pm)")))?;
    let now = Local::now();
    let resolve = |date: NaiveDate| {
        let dt = date.and_time(naive);
        match Local.from_local_datetime(&dt) {
            chrono::LocalResult::Single(t) | chrono::LocalResult::Ambiguous(t, _) => Ok(t),
            chrono::LocalResult::None => Err(ClixError::Usage(format!(
                "could not resolve local time '{s}'"
            ))),
        }
    };
    let today = resolve(now.date_naive())?;
    let target = if today > now {
        today
    } else {
        let tomorrow = now
            .date_naive()
            .succ_opt()
            .ok_or_else(|| ClixError::Usage("date overflow".into()))?;
        resolve(tomorrow)?
    };
    Ok(SystemTime::from(target))
}
