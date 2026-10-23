mod actions;
mod cases;
mod claude;
mod config;
mod dev;
mod highlight;
mod lang;
mod mock;
mod proc;
mod render;
mod settings;
mod srs;
mod stats;
mod store;
mod sync;
mod testing;
mod tui;
mod xdg;

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use chrono::Local;
use clap::{Parser, Subcommand, ValueEnum};

use crate::claude::Kind;
use crate::config::Config;
use crate::proc::Cancel;
use crate::render::sanitize;
use crate::store::{MockRecord, Problem, Status, Store};

/// Minimum timeout for `neet test` runs from the CLI (includes compiling).
const MIN_TEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Parser)]
#[command(
    name = "neet",
    version,
    about = "Practise NeetCode 150/250 offline in your terminal, with Claude as a coach"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

/// Which problems to pick from.
#[derive(clap::Args, Clone)]
struct Filter {
    #[arg(long, short, value_enum, default_value_t = ListArg::Nc150)]
    list: ListArg,
    /// Only this pattern (substring match, e.g. "graph")
    #[arg(long, short)]
    pattern: Option<String>,
    #[arg(long, short, value_enum)]
    difficulty: Option<Difficulty>,
}

impl Filter {
    fn matches(&self, p: &Problem) -> bool {
        self.list.contains(p)
            && self
                .pattern
                .as_ref()
                .is_none_or(|q| p.pattern.to_lowercase().contains(&q.to_lowercase()))
            && self
                .difficulty
                .is_none_or(|d| p.difficulty.eq_ignore_ascii_case(d.name()))
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Download problem lists, statements and NeetCode solutions for offline use
    Sync {
        /// Refetch problem statements even if cached
        #[arg(long)]
        force: bool,
    },
    /// List problems with their status
    List {
        #[command(flatten)]
        filter: Filter,
    },
    /// Print a problem statement
    Show { problem: String },
    /// Open a problem in your editor
    Edit { problem: String },
    /// Run your solution file as-is
    Run { problem: String },
    /// Test your solution against the examples and edge cases (Claude writes the tests once)
    Test {
        problem: String,
        /// Throw away the cached tests and generate new ones
        #[arg(long)]
        regen: bool,
    },
    /// Ask Claude for the next nudge (accepts a solution file path, handy from nvim)
    Hint { problem: String },
    /// Have Claude review your solution
    Review { problem: String },
    /// Ask Claude a question about a problem
    Ask {
        problem: String,
        question: Vec<String>,
    },
    /// Print the NeetCode solution
    Solution { problem: String },
    /// Mark a problem as solved (or a due review as done), or --undo
    Done {
        problem: String,
        #[arg(long)]
        undo: bool,
    },
    /// Problems due for a spaced-repetition review
    Due,
    /// Pick the next unsolved problem in roadmap order (or a random one)
    Next {
        #[command(flatten)]
        filter: Filter,
        #[arg(long, short)]
        random: bool,
        /// Open it in your editor right away
        #[arg(long, short)]
        edit: bool,
    },
    /// Timed mock interview: no hints, then a debrief with Claude as the interviewer
    Mock {
        /// Problem to use (default: a random unsolved one from the filter)
        problem: Option<String>,
        #[command(flatten)]
        filter: Filter,
    },
    /// Archive your current solution and start over from the starter code
    Fresh { problem: String },
    /// Edit your notes for a problem, or export all notes as a cheat sheet
    Notes {
        problem: Option<String>,
        /// Print all notes as one Markdown cheat sheet
        #[arg(long)]
        export: bool,
    },
    /// Show progress, weak spots and mock results
    Stats {
        #[arg(long, short, value_enum, default_value_t = ListArg::Nc150)]
        list: ListArg,
        /// Ask Claude for a weak-spot analysis and study plan
        #[arg(long)]
        coach: bool,
    },
    /// Show where config, data and solutions live
    Paths,
    /// Open the config file in your editor (or change settings in the TUI with `,`)
    Config,
    /// Maintainer tools
    #[command(hide = true, subcommand)]
    Dev(DevCmd),
}

#[derive(Subcommand)]
enum DevCmd {
    /// Run the built-in tests against NeetCode's reference solutions
    #[command(name = "check-tests")]
    Check {
        /// Only these problems (slugs)
        problems: Vec<String>,
    },
    /// Generate edge cases into data/tests.json (inputs by Claude, outputs by the reference)
    #[command(name = "gen-tests")]
    Gen {
        /// Only these problems (slugs)
        problems: Vec<String>,
        /// Model for writing the inputs
        #[arg(long, default_value = "sonnet")]
        model: String,
        /// Regenerate problems that already have edge cases
        #[arg(long)]
        force: bool,
        #[arg(long, short, default_value_t = 4)]
        jobs: usize,
        #[arg(long, default_value = "data/tests.json")]
        file: std::path::PathBuf,
    },
    /// Rewrite data/tests.json in its canonical layout, dropping oversized cases
    #[command(name = "tidy-tests")]
    Tidy {
        #[arg(long, default_value = "data/tests.json")]
        file: std::path::PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ListArg {
    #[value(name = "150")]
    Nc150,
    #[value(name = "250")]
    Nc250,
    #[value(name = "blind75")]
    Blind75,
    All,
}

impl ListArg {
    fn contains(self, p: &Problem) -> bool {
        match self {
            ListArg::Nc150 => p.nc150,
            ListArg::Nc250 => p.nc250,
            ListArg::Blind75 => p.blind75,
            ListArg::All => true,
        }
    }

    fn name(self) -> &'static str {
        match self {
            ListArg::Nc150 => "NeetCode 150",
            ListArg::Nc250 => "NeetCode 250",
            ListArg::Blind75 => "Blind 75",
            ListArg::All => "All",
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Difficulty {
    Easy,
    Medium,
    Hard,
}

impl Difficulty {
    fn name(self) -> &'static str {
        match self {
            Difficulty::Easy => "Easy",
            Difficulty::Medium => "Medium",
            Difficulty::Hard => "Hard",
        }
    }
}

fn main() {
    // Behave like a normal CLI when piped into `head` etc.
    // SAFETY: called before any threads exist; restores the default disposition.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    if let Err(e) = real_main() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    // Before loading the config, so a broken config can still be fixed this way.
    if let Some(Cmd::Config) = cli.command {
        return edit_config();
    }
    let cfg = Config::load()?;
    let store = Store::new(&cfg)?;

    let Some(cmd) = cli.command else {
        if !store.has_data() {
            println!("No local data yet, running `neet sync` first.\n");
            sync::run(&store, &cfg, false)?;
        }
        return tui::run(cfg, store);
    };

    if let Cmd::Sync { force } = cmd {
        return sync::run(&store, &cfg, force);
    }
    if let Cmd::Paths = cmd {
        println!("config     {}", Config::path().display());
        println!("data       {}", store.root.display());
        println!("solutions  {}", store.workspace.display());
        return Ok(());
    }

    let problems = store.load_problems()?;
    let progress = store.load_progress()?;
    let find = |q: &str| actions::resolve(&problems, &store, q).cloned();
    match cmd {
        Cmd::Sync { .. } | Cmd::Paths | Cmd::Config => unreachable!(),
        Cmd::Dev(DevCmd::Check { problems: only }) => {
            dev::check_tests(&store, &problems, &only)?;
        }
        Cmd::Dev(DevCmd::Gen {
            problems: only,
            model,
            force,
            jobs,
            file,
        }) => {
            dev::gen_tests(&store, &problems, &only, Some(model), force, jobs, &file)?;
        }
        Cmd::Dev(DevCmd::Tidy { file }) => dev::tidy_tests(&file)?,
        Cmd::List { filter } => {
            let today = srs::today();
            let mut last = "";
            for p in problems.iter().filter(|p| filter.matches(p)) {
                if p.pattern != last {
                    println!("\n{}", p.pattern);
                    last = &p.pattern;
                }
                let entry = progress.entries.get(&p.slug);
                let icon = match entry.map(|e| e.status).unwrap_or_default() {
                    Status::Solved if entry.is_some_and(|e| srs::is_due(e, today)) => "⟳",
                    Status::Solved => "✓",
                    Status::Attempted => "●",
                    Status::Todo => " ",
                };
                println!("  {icon} {:<6} {:<50} {}", p.difficulty, p.title, p.slug);
            }
        }
        Cmd::Show { problem } => {
            let p = find(&problem)?;
            print!(
                "{}",
                sanitize(&store.load_description(&p.slug).unwrap_or_default())
            );
        }
        Cmd::Edit { problem } => {
            actions::edit(&store, &cfg, &find(&problem)?, cfg.lsp_enabled(false))?;
        }
        Cmd::Run { problem } => {
            let p = find(&problem)?;
            let status =
                actions::run_interactive(&cfg, &store, &store.solution_path(&p, cfg.lang()))?;
            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
        }
        Cmd::Test { problem, regen } => {
            let p = find(&problem)?;
            let test_timeout = (cfg.run_timeout() * 2).max(MIN_TEST_TIMEOUT);
            let claude = testing::needs_claude(&store, &cfg, &p, regen);
            if claude {
                eprintln!("Claude is writing tests for {} (once)...", p.title);
            }
            let (output, result) =
                actions::run_tests(&store, &cfg, &p, regen, test_timeout, &Cancel::default())?;
            println!("{}", sanitize(&output));
            println!("\n{}", testing::summary(&output));
            match cases::spec(&store, &p) {
                cases::Availability::Ready(spec)
                    if !claude && testing::builtin_harness(&store, &cfg, &p).is_some() =>
                {
                    eprintln!(
                        "(built-in tests: {}; --regen has Claude write tests instead)",
                        spec.describe()
                    );
                }
                _ => eprintln!(
                    "(tests: {}; edit it or use --regen if a test looks wrong)",
                    store.harness_path(&p, cfg.lang()).display()
                ),
            }
            if !testing::all_passed(result) {
                std::process::exit(1);
            }
        }
        Cmd::Hint { problem } => coach(&store, &cfg, &find(&problem)?, Kind::Hint, "")?,
        Cmd::Review { problem } => coach(&store, &cfg, &find(&problem)?, Kind::Review, "")?,
        Cmd::Ask { problem, question } => {
            let q = question.join(" ");
            if q.trim().is_empty() {
                bail!("usage: neet ask <problem> <question...>");
            }
            coach(&store, &cfg, &find(&problem)?, Kind::Ask, &q)?;
        }
        Cmd::Solution { problem } => {
            let p = find(&problem)?;
            actions::mark_revealed(&store, &p)?;
            match store.load_article(&p.slug) {
                Some(a) => print!("{}", sanitize(&render::filter_article(&a, cfg.lang()))),
                None => match store.load_ref_solution(&p.slug, cfg.lang()) {
                    Some(code) => print!("{}", sanitize(&code)),
                    None => println!("No offline solution available for {}.", p.title),
                },
            }
        }
        Cmd::Done { problem, undo } => {
            let p = find(&problem)?;
            let (_, outcome) = actions::set_solved(&store, &p, !undo)?;
            println!("{}", outcome.describe(&p.title));
        }
        Cmd::Due => {
            let today = srs::today();
            let mut due: Vec<(&Problem, chrono::NaiveDate)> = problems
                .iter()
                .filter_map(|p| {
                    let e = progress.entries.get(&p.slug)?;
                    srs::is_due(e, today).then_some((p, e.next_review?))
                })
                .collect();
            due.sort_by_key(|(_, d)| *d);
            if due.is_empty() {
                println!("Nothing due today.");
                if let Some(next) = progress
                    .entries
                    .values()
                    .filter_map(|e| e.next_review)
                    .min()
                {
                    println!("Next review: {next}");
                }
            }
            for (p, d) in due {
                println!("  ⟳ {:<6} {:<50} due since {d}", p.difficulty, p.title);
            }
        }
        Cmd::Next {
            filter,
            random,
            edit,
        } => {
            let include = |p: &Problem| filter.matches(p);
            let pick = if random {
                actions::random_unsolved(&problems, &progress, include)
            } else {
                actions::next_unsolved(&problems, &progress, include, None)
            };
            let Some(i) = pick else {
                println!("Everything matching is solved. Nice!");
                return Ok(());
            };
            let p = &problems[i];
            println!(
                "{} ({}, {})  neet edit {}",
                p.title, p.difficulty, p.pattern, p.slug
            );
            if edit {
                actions::edit(&store, &cfg, p, cfg.lsp_enabled(false))?;
            }
        }
        Cmd::Mock { problem, filter } => {
            let p = match problem {
                Some(q) => find(&q)?,
                None => {
                    let i = actions::random_unsolved(&problems, &progress, |p| filter.matches(p))
                        .ok_or_else(|| anyhow::anyhow!("no unsolved problem matches"))?;
                    problems[i].clone()
                }
            };
            mock_cli(&store, &cfg, &p)?;
        }
        Cmd::Fresh { problem } => {
            let p = find(&problem)?;
            match actions::fresh_attempt(&store, &cfg, &p)? {
                Some(old) => println!("Previous attempt archived to {}", old.display()),
                None => println!("No previous attempt; created a fresh starter file."),
            }
        }
        Cmd::Notes { problem, export } => {
            if export {
                print!("{}", actions::export_notes(&store, &problems));
            } else {
                let Some(q) = problem else {
                    bail!("usage: neet notes <problem>  or  neet notes --export");
                };
                actions::edit_notes(&store, &cfg, &find(&q)?)?;
            }
        }
        Cmd::Stats { list, coach } => {
            let today = srs::today();
            bars(&problems, &progress);
            let report = stats::report(
                &problems,
                &progress,
                &store.load_mocks()?,
                list.name(),
                |p| list.contains(p),
                today,
            );
            println!("\n{report}");
            if coach {
                eprintln!("Claude is looking at your numbers...\n");
                let text = stream_claude(&cfg, Kind::Coach, &stats::coach_prompt(&report))?;
                std::fs::write(store.coach_path(), &text)?;
            }
        }
    }
    Ok(())
}

/// Opens the config file in the editor (from the config if it loads, else $EDITOR/nvim)
/// and checks it afterwards.
fn edit_config() -> Result<()> {
    let path = Config::path();
    // Creates the default file if there is none yet.
    let editor = match Config::load() {
        Ok(cfg) => cfg.editor(),
        Err(e) => {
            eprintln!("note: the config currently has an error: {e:#}");
            std::env::var("EDITOR")
                .ok()
                .filter(|e| !e.trim().is_empty())
                .unwrap_or_else(|| "nvim".into())
        }
    };
    let mut parts = editor.split_whitespace();
    let status = std::process::Command::new(parts.next().unwrap_or("nvim"))
        .args(parts)
        .arg(&path)
        .status()?;
    if !status.success() {
        bail!("editor exited with {status}");
    }
    match Config::load() {
        Ok(_) => Ok(()),
        Err(e) => bail!("the config doesn't load, please fix it (neet config): {e:#}"),
    }
}

/// Runs Claude, streaming sanitized text to stdout; returns the full text.
fn stream_claude(cfg: &Config, kind: Kind, prompt: &str) -> Result<String> {
    let mut stdout = std::io::stdout();
    let text = claude::run(cfg, kind, prompt, &Cancel::default(), |d| {
        let _ = stdout.write_all(sanitize(d).as_bytes());
        let _ = stdout.flush();
    })?;
    println!();
    Ok(text)
}

fn coach(store: &Store, cfg: &Config, p: &Problem, kind: Kind, question: &str) -> Result<()> {
    let prompt = actions::prompt_for(store, cfg, p, kind, question)?;
    eprintln!("Claude is thinking about {}...\n", p.title);
    let text = stream_claude(cfg, kind, &prompt)?;
    let (verdict, solve) = actions::record_ai(store, p, kind, question, &text)?;
    match (verdict, solve) {
        (Some(true), Some(s)) => println!("\n{}", s.describe(&p.title)),
        (Some(false), _) => println!("\nNot quite there yet. Keep going!"),
        _ => {}
    }
    Ok(())
}

fn read_line(prompt: &str) -> Result<Option<String>> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim().to_string()))
}

fn mock_cli(store: &Store, cfg: &Config, p: &Problem) -> Result<()> {
    let limit = mock::time_limit(cfg, p);
    println!(
        "Mock interview: {} ({}, {})\nTime limit {}. No hints, no solution. Your statement opens \
         next to a fresh file; quit the editor when you're done.",
        p.title,
        p.difficulty,
        p.pattern,
        mock::fmt_duration(limit)
    );
    if read_line("Press Enter to start the clock (q to cancel) ")?.as_deref() == Some("q") {
        return Ok(());
    }
    if let Some(old) = actions::fresh_attempt(store, cfg, p)? {
        println!("(previous attempt archived to {})", old.display());
    }
    let started = Local::now();
    let clock = Instant::now();
    loop {
        actions::edit(store, cfg, p, cfg.lsp_enabled(true))?;
        let elapsed = clock.elapsed();
        println!(
            "\nTime: {} of {}{}",
            mock::fmt_duration(elapsed),
            mock::fmt_duration(limit),
            if elapsed > limit { " (over time)" } else { "" }
        );
        match read_line("[d]ebrief, [e]dit again, [s]kip debrief: ")?.as_deref() {
            Some("e") => continue,
            Some("s") | None => {
                record_mock(store, p, started, elapsed, limit, None)?;
                return Ok(());
            }
            _ => break,
        }
    }
    let elapsed = clock.elapsed();
    let ctx = claude::Coaching::gather(store, cfg, p);
    let mut transcript = mock::Transcript::default();
    println!(
        "\nDebrief. Answer the interviewer; type {} to wrap up.\n",
        mock::DONE
    );
    loop {
        let prompt = mock::debrief_prompt(&ctx, elapsed, limit, &transcript);
        print!("Interviewer: ");
        let reply = stream_claude(cfg, Kind::Mock, &prompt)?;
        transcript.push(mock::Speaker::Interviewer, reply);
        if transcript.finished() || transcript.gave_up() {
            break;
        }
        let answer = read_line("\nYou: ")?.unwrap_or_else(|| mock::DONE.to_string());
        transcript.push(mock::Speaker::Candidate, answer);
        println!();
    }
    let result = transcript
        .turns
        .last()
        .and_then(|(_, t)| mock::result_of(t));
    let md = transcript.to_markdown();
    actions::record_ai(store, p, Kind::Mock, "", &md)?;
    record_mock(store, p, started, elapsed, limit, result)
}

fn record_mock(
    store: &Store,
    p: &Problem,
    started: chrono::DateTime<Local>,
    elapsed: Duration,
    limit: Duration,
    result: Option<String>,
) -> Result<()> {
    store.add_mock(MockRecord {
        slug: p.slug.clone(),
        title: p.title.clone(),
        difficulty: p.difficulty.clone(),
        started,
        elapsed_secs: elapsed.as_secs(),
        limit_secs: limit.as_secs(),
        result,
    })
}

fn bars(problems: &[Problem], progress: &store::Progress) {
    let solved = |p: &Problem| {
        progress
            .entries
            .get(&p.slug)
            .is_some_and(|e| e.status == Status::Solved)
    };
    for list in [ListArg::Blind75, ListArg::Nc150, ListArg::Nc250] {
        let in_list: Vec<&Problem> = problems.iter().filter(|p| list.contains(p)).collect();
        let done = in_list.iter().filter(|p| solved(p)).count();
        println!(
            "{:<14} {}  {done}/{}",
            list.name(),
            bar(done, in_list.len(), 30),
            in_list.len()
        );
    }
}

fn bar(done: usize, total: usize, width: usize) -> String {
    let filled = (done * width).checked_div(total).unwrap_or(0).min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}
