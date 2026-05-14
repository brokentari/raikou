//! Layer 1: case files run through `redis-cli` on both servers.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use std::{fs, thread};

use crate::oracle::{Environment, Target};

#[derive(Clone, Copy)]
enum Compare {
    Exact,
    Unordered,
    Ignore,
}

enum Step {
    Command {
        line: usize,
        text: String,
        compare: Compare,
    },
    Sleep(u64),
}

struct Case {
    path: PathBuf,
    steps: Vec<Step>,
    resp2_only: bool,
}

struct Block {
    line: usize,
    command: String,
    compare: Compare,
}

enum Action {
    Line(String),
    Sleep(u64),
}

struct Reply {
    line: usize,
    command: String,
    text: String,
}

fn load(path: &Path) -> Result<Case, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut steps = Vec::new();
    let mut resp2_only = false;
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "@resp2-only" {
            resp2_only = true;
            continue;
        }
        let step = if let Some(rest) = trimmed.strip_prefix("@sleep ") {
            let ms = rest
                .trim()
                .parse()
                .map_err(|_| format!("{}:{line}: bad @sleep", path.display()))?;
            Step::Sleep(ms)
        } else if let Some(rest) = trimmed.strip_prefix("@unordered ") {
            Step::Command {
                line,
                text: rest.trim().to_owned(),
                compare: Compare::Unordered,
            }
        } else if let Some(rest) = trimmed.strip_prefix("@ignore ") {
            Step::Command {
                line,
                text: rest.trim().to_owned(),
                compare: Compare::Ignore,
            }
        } else if trimmed.starts_with('@') {
            return Err(format!("{}:{line}: unknown directive", path.display()));
        } else {
            Step::Command {
                line,
                text: trimmed.to_owned(),
                compare: Compare::Exact,
            }
        };
        steps.push(step);
    }
    Ok(Case {
        path: path.to_owned(),
        steps,
        resp2_only,
    })
}

fn marker(id: usize) -> String {
    format!("raikou-marker-{id}")
}

fn first_word(text: &str) -> String {
    text.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase()
}

fn script(case: &Case) -> Result<(Vec<Action>, Vec<Block>), String> {
    let setup = Block {
        line: 0,
        command: "FLUSHALL".to_owned(),
        compare: Compare::Exact,
    };
    let mut blocks = vec![setup];
    let mut actions = vec![
        Action::Line("FLUSHALL".to_owned()),
        Action::Line(format!("ECHO {}", marker(0))),
    ];
    let mut open: Option<Block> = None;
    for step in &case.steps {
        match step {
            Step::Sleep(ms) => actions.push(Action::Sleep(*ms)),
            Step::Command {
                line,
                text,
                compare,
            } => {
                actions.push(Action::Line(text.clone()));
                let word = first_word(text);
                if open.is_some() {
                    if word != "EXEC" && word != "DISCARD" {
                        continue;
                    }
                    blocks.extend(open.take());
                } else if word == "MULTI" {
                    let command = format!("{text} ... (transaction)");
                    open = Some(Block {
                        line: *line,
                        command,
                        compare: *compare,
                    });
                    continue;
                } else {
                    blocks.push(Block {
                        line: *line,
                        command: text.clone(),
                        compare: *compare,
                    });
                }
                actions.push(Action::Line(format!("ECHO {}", marker(blocks.len() - 1))));
            }
        }
    }
    if open.is_some() {
        return Err(format!(
            "{}: MULTI without EXEC or DISCARD",
            case.path.display()
        ));
    }
    Ok((actions, blocks))
}

fn drain<R: Read + Send + 'static>(mut source: R) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = source.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

fn strip_index(line: &str) -> &str {
    let rest = line.trim_start();
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    match rest.as_bytes().get(digits..digits + 2) {
        Some([b')' | b'#', b' ']) if digits > 0 => &rest[digits + 2..],
        _ => line,
    }
}

fn run(env: &Environment, case: &Case, target: &Target, resp3: bool) -> Result<Vec<Reply>, String> {
    let (actions, blocks) = script(case)?;
    let mut command = Command::new("docker");
    command.args([
        "exec",
        "-i",
        env.container(),
        "redis-cli",
        "-p",
        &target.port.to_string(),
    ]);
    if resp3 {
        command.arg("-3");
    }
    let mut child = command
        .arg("--no-raw")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run redis-cli: {error}"))?;
    let stdout = drain(child.stdout.take().ok_or("no stdout")?);
    let stderr = drain(child.stderr.take().ok_or("no stderr")?);
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    for action in &actions {
        let sent = match action {
            Action::Line(text) => writeln!(stdin, "{text}").and_then(|()| stdin.flush()),
            Action::Sleep(ms) => {
                thread::sleep(Duration::from_millis(*ms));
                Ok(())
            }
        };
        if sent.is_err() {
            break;
        }
    }
    drop(stdin);
    let output = stdout.join().map_err(|_| "stdout reader panicked")?;
    let errors = stderr.join().map_err(|_| "stderr reader panicked")?;
    let _ = child.wait();
    let mut replies = split(&blocks, &output, &errors);
    replies.remove(0);
    Ok(replies)
}

fn split(blocks: &[Block], output: &str, errors: &str) -> Vec<Reply> {
    let mut replies = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut lines = output.lines();
    for (id, block) in blocks.iter().enumerate() {
        let end = format!("\"{}\"", marker(id));
        let mut found = false;
        for line in lines.by_ref() {
            if line == end {
                found = true;
                break;
            }
            current.push(line);
        }
        let text = if found || !current.is_empty() {
            normalize(block, &current)
        } else {
            "<no reply>".to_owned()
        };
        replies.push(Reply {
            line: block.line,
            command: block.command.clone(),
            text,
        });
        current.clear();
    }
    if !errors.trim().is_empty() {
        replies.push(Reply {
            line: 0,
            command: "(stderr)".to_owned(),
            text: errors.trim().to_owned(),
        });
    }
    replies
}

fn normalize(block: &Block, lines: &[&str]) -> String {
    match block.compare {
        Compare::Exact => lines.join("\n"),
        Compare::Ignore => "<ignored>".to_owned(),
        Compare::Unordered => {
            let mut sorted: Vec<&str> = lines.iter().map(|line| strip_index(line)).collect();
            sorted.sort_unstable();
            sorted.join("\n")
        }
    }
}

fn cases(root: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = root.join("tests/cases/cli");
    let entries = fs::read_dir(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "txt"))
        .collect();
    paths.sort();
    if let Ok(filter) = std::env::var("RAIKOU_COMPAT_FILTER") {
        paths.retain(|path| path.to_string_lossy().contains(&filter));
    }
    Ok(paths)
}

pub(crate) fn run_all(env: &Environment, root: &Path) -> Result<usize, String> {
    let mut failures = 0;
    for path in cases(root)? {
        let case = load(&path)?;
        let shown = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        for (label, resp3) in [("RESP2", false), ("RESP3", true)] {
            if resp3 && case.resp2_only {
                continue;
            }
            let expected = run(env, &case, &env.oracle, resp3)?;
            let actual = run(env, &case, &env.subject_target, resp3)?;
            let mut mismatches = 0;
            for (want, got) in expected.iter().zip(&actual) {
                if want.text != got.text {
                    mismatches += 1;
                    println!("FAIL {shown}:{} [{label}] {}", want.line, want.command);
                    println!("  {}:\n{}", env.oracle.name, indent(&want.text));
                    println!("  {}:\n{}", env.subject_target.name, indent(&got.text));
                }
            }
            if expected.len() != actual.len() {
                mismatches += 1;
                println!(
                    "FAIL {shown} [{label}] reply counts differ: {} vs {}",
                    expected.len(),
                    actual.len()
                );
            }
            failures += mismatches;
            println!(
                "{} {shown} [{label}] {} replies",
                if mismatches == 0 { "ok  " } else { "FAIL" },
                expected.len()
            );
        }
    }
    Ok(failures)
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}
