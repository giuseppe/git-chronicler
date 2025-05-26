/*
 * git-chronicler
 *
 * Copyright (C) 2025 Giuseppe Scrivano <giuseppe@scrivano.org>
 * git-chronicler is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * git-chronicler is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with git-chronicler.  If not, see <http://www.gnu.org/licenses/>.
 *
 */

use clap::{Parser, Subcommand};
use codehawk::openai::{
    Message, Opts, ToolCallback, ToolItem, ToolsCollection, make_message, post_request,
};
use env_logger::Env;
use log::{debug, info, trace};
use regex::Regex;
use serde::Deserialize;
use std::error::Error;
use std::io::Write;
use std::process::{Command, Stdio};

const DEFAULT_OPENAI_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const MODEL: &str = "google/gemini-2.5-pro-preview-03-25";
const MAX_TOKENS: u32 = 16384;

/// Creates a prompt for the AI model to improve an existing commit message.
fn inline_prompt() -> String {
    debug!("Creating inline prompt for commit message improvement");
    "Improve the git commit message for the patch and add any missing information you get from the code.  \
     Explain why a change is done, not what was changed.  Keep the first line below 52 columns and next ones under 80 columns.  \
     Return only the git commit message without any other information nor any delimiter.  \
     Leave unchanged any signed-off line or any other trailer:\n".to_owned()
}

/// Creates a prompt for the AI model to write a new commit message.
fn write_prompt() -> String {
    debug!("Creating write prompt for new commit message");
    "Write the git commit message for the patch and add any information you get from the code.  \
     Explain why a change is done, not what was changed.  Keep the first line below 52 columns and next ones under 80 columns.  \
     Return only the git commit message without any other information nor any delimiter:\n".to_owned()
}

/// Creates a prompt for the AI model to check an existing commit message for errors.
fn check_prompt() -> String {
    debug!("Creating check prompt for commit message validation");
    "Report any mistake you see in the commit log message.  \
     If the input contains a significant error or discrepancy, the first line of the returned message must only contain the string ERROR and nothing more.  \
     Ignore the date and the author information, look only at the commit message.  \
     Explain carefully what changes you suggest:\n".to_owned()
}

/// entrypoint for the list_all_files tool
/// This code is copied from codehawk for now
fn tool_list_all_files(_params_str: &String) -> Result<String, Box<dyn Error>> {
    debug!("Executing list_all_files tool");
    run_git_command(vec!["ls-files"])
}

/// Creates a prompt to ask for a summary of the current branch
fn summary_prompt() -> String {
    debug!("Creating summary prompt");

    "Summarize the changes in the git commits, give more importance to the commit messages.\n \
     It is used as the description for a pull request.\n\
     Provide first a one-line descriptive title.\n\
     \n"
    .to_owned()
}

/// entrypoint for the read_file tool
/// This code is copied from codehawk for now
fn tool_read_file(params_str: &String) -> Result<String, Box<dyn Error>> {
    #[derive(Deserialize)]
    struct Params {
        path: String,
    }

    let params: Params = serde_json::from_str::<Params>(params_str)?;

    run_git_command(vec!["show", "HEAD:{}", &params.path])
}

fn append_tool(tools: &mut ToolsCollection, name: String, callback: ToolCallback, schema: String) {
    debug!("Adding tool: {}", name);
    let item = ToolItem { callback, schema };
    tools.insert(name, item);
}

fn initialize_tools() -> ToolsCollection {
    debug!("Initializing tools for AI request");
    let mut tools: ToolsCollection = ToolsCollection::new();

    append_tool(
        &mut tools,
        "read_file".to_string(),
        tool_read_file,
        r#"
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Get the content of a file stored in the repository.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "path of the file under the repository, e.g. src/main.rs"
                        }
                    },
                    "required": [
                        "path"
                    ],
                    "additionalProperties": false
                }
            }
        }
"#
        .to_string(),
    );

    append_tool(
        &mut tools,
        "list_all_files".to_string(),
        tool_list_all_files,
        r#"
        {
            "type": "function",
            "function": {
                "name": "list_all_files",
                "description": "Get the list of all the files in the repository.",
                "parameters": {
                    "type": "object",
                    "properties": {
                    },
                    "required": [
                    ],
                    "additionalProperties": false
                }
            }
        }
"#
        .to_string(),
    );

    debug!("Tools initialization completed with {} tools", tools.len());
    tools
}

/// Run a git command and retrieve the stdout
fn run_git_command(args: Vec<&str>) -> Result<String, Box<dyn Error>> {
    debug!("Running git command {:?}", args);

    let mut input = Command::new("git");
    input.args(args);

    let output = input.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8(output.stderr)?;
        let err: Box<dyn Error> = stderr.into();
        return Err(err);
    }
    let r = String::from_utf8(output.stdout)?;
    debug!("Successfully run command and got {:?}", r);
    Ok(r)
}

/// Get the last N git commit messages and strip any trailer information
fn get_last_git_messages(n: u64) -> Result<Vec<String>, Box<dyn Error>> {
    let n_arg = format!("-n{}", n);
    let args: Vec<&str> = vec!["log", &n_arg, "--no-merges", "--pretty=format:%B%x00"];

    let out = run_git_command(args)?;

    let trailer_regex = Regex::new(r"^[A-Za-z0-9-]+:\s+.+$")?;

    let messages = out
        .split('\0')
        .map(|msg| {
            msg.lines()
                .take_while(|line| !trailer_regex.is_match(line.trim()))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string()
        })
        .filter(|message| message.len() > 0)
        .collect::<Vec<_>>();

    Ok(messages)
}

/// Retrieves the changes in the current branch to prepare a summary
fn get_branch_patches(base: &String) -> Result<String, Box<dyn Error>> {
    let arg = format!("{}..HEAD", base);
    run_git_command(vec!["log", "-p", &arg])
}

/// Retrieves the last commit log message and patch using `git log -p -1`.
fn get_last_commit() -> Result<String, Box<dyn Error>> {
    run_git_command(vec!["log", "-p", "-1"])
}

/// Retrieves the diff of changes using `git diff`.
fn get_diff(cached: bool) -> Result<String, Box<dyn Error>> {
    let mut args: Vec<&str> = vec!["diff", "-U50"];

    if cached {
        args.push("--cached");
        debug!("Using staged changes only");
    } else {
        debug!("Using unstaged changes");
    }

    let r = run_git_command(args)?;
    if r.is_empty() {
        return Err("Empty diff returned - no changes detected".into());
    }
    Ok(r)
}

/// Creates a new commit using the provided commit message.
fn write_commit(
    commit_msg: &str,
    signoff: bool,
    cached: bool,
    interactive: bool,
) -> Result<(), Box<dyn Error>> {
    info!(
        "Creating new commit with signoff={}, cached={}, interactive={}",
        signoff, cached, interactive
    );

    let mut git_cmd = Command::new("git");
    let mut cmd = git_cmd.arg("commit");
    if !cached {
        cmd = cmd.arg("-a");
        debug!("Using -a flag to commit all changes");
    }
    if signoff {
        cmd = cmd.arg("-s");
        debug!("Adding signoff to commit");
    }

    if interactive {
        debug!("Interactive mode enabled, preparing temporary file");
        let tempfile = tempfile::NamedTempFile::new()?;
        let path = tempfile.path().to_str().ok_or("invalid temp file name")?;

        trace!("Writing commit message to temporary file: {}", path);
        std::fs::write(tempfile.path(), commit_msg.as_bytes())?;

        cmd = cmd.args(["-F", path, "--edit"]);
        debug!("Launching editor for commit message");

        trace!("Running git command: {:?}", cmd);
        let mut child = cmd.spawn()?;
        let status = child.wait()?;

        if !status.success() {
            return Err("Commit command failed".into());
        }

        info!("Commit created successfully");
        Ok(())
    } else {
        // Read from stdin if it is not running in interactive mode
        debug!("Non-interactive mode, using stdin for commit message");
        cmd = cmd.args(["-F", "-"]);

        trace!("Running git command: {:?}", cmd);
        let mut child = cmd.stdin(Stdio::piped()).spawn()?;
        if let Some(stdin) = child.stdin.as_mut() {
            trace!(
                "Writing commit message to stdin ({} bytes)",
                commit_msg.len()
            );
            stdin.write_all(commit_msg.as_bytes())?;
        } else {
            return Err("Failed to open stdin for git commit".into());
        }

        let output = child.wait_with_output()?;
        if !output.status.success() {
            let stderr = String::from_utf8(output.stderr)?;
            let err: Box<dyn Error> = stderr.into();
            return Err(err);
        }

        info!("Commit created successfully");
        Ok(())
    }
}

/// Amends the last commit with the provided commit message.
fn amend_commit(commit_msg: &str) -> Result<(), Box<dyn Error>> {
    info!("Amending last commit");
    debug!("Commit message length: {} bytes", commit_msg.len());

    let mut child = Command::new("git")
        .args(["commit", "--amend", "-F", "-"])
        .stdin(Stdio::piped())
        .spawn()?;

    trace!("Writing commit message to stdin for amend");
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(commit_msg.as_bytes())?;
    } else {
        return Err("Failed to open stdin for git amend".into());
    }

    let output = child.wait_with_output()?;

    if !output.status.success() {
        let stderr = String::from_utf8(output.stderr)?;
        let err: Box<dyn Error> = stderr.into();
        return Err(err);
    }

    info!("Commit amended successfully");
    Ok(())
}

/// Checks the AI's response for the 'check' command.
fn check_commit(msg: &str) -> Result<(), Box<dyn Error>> {
    debug!("Checking commit message for errors");
    if let Some(msg) = msg.strip_prefix("ERROR\n") {
        eprintln!("{}", msg.trim());
        return Err("wrong commit message".into());
    }
    debug!("Commit message passed validation check");
    println!("{}", &msg);
    Ok(())
}

#[derive(Parser, Debug)]
#[clap(version = env!("CARGO_PKG_VERSION"))]
struct CliOpts {
    /// Maximum number of tokens to generate
    #[clap(short, long)]
    max_tokens: Option<u32>,
    #[clap(long)]
    /// Override the model to use
    model: Option<String>,
    #[clap(long, default_value = DEFAULT_OPENAI_URL)]
    /// Override the endpoint URL to use
    endpoint: String,
    #[clap(subcommand)]
    command: SubCommand,
}

#[derive(Debug, Subcommand)]
enum SubCommand {
    /// Write a commit message
    Write {
        /// Add a Signed-off-by trailer by the committer at the end of the commit log message
        #[clap(short, long)]
        signoff: bool,

        /// Commit only the staged changes
        #[clap(long)]
        cached: bool,

        /// Modify the message before commit
        #[clap(short, long)]
        interactive: bool,
    },
    /// Fixup the current commit message inline
    Fixup,
    /// Check if the commit message describes correctly the patch
    Check,
    /// Create a summary of the current branch
    Summary {
        /// Base branch
        base: String,
    },
}

/// Main entry point for the git-chronicler application.
fn main() -> Result<(), Box<dyn Error>> {
    // Initialize environment logger with custom configuration
    let env = Env::new()
        .filter_or("RUST_LOG", "warning")
        .write_style_or("LOG_STYLE", "always");

    env_logger::init_from_env(env);
    debug!("Logging initialized");

    let opts = CliOpts::parse();
    debug!("Command line options parsed");

    let model = opts.model.clone().unwrap_or_else(|| MODEL.to_string());
    debug!("Using model: {}", model);
    debug!("Using endpoint: {}", opts.endpoint);

    let (prompt, patch) = match opts.command {
        SubCommand::Fixup => {
            info!("Running fixup command to improve existing commit message");
            (inline_prompt(), get_last_commit()?)
        }
        SubCommand::Check => {
            info!("Running check command to validate commit message");
            (check_prompt(), get_last_commit()?)
        }
        SubCommand::Write {
            signoff,
            cached,
            interactive,
        } => {
            info!("Running write command to create new commit message");
            debug!(
                "Write options: signoff={}, cached={}, interactive={}",
                signoff, cached, interactive
            );
            (write_prompt(), get_diff(cached)?)
        }
        SubCommand::Summary { ref base } => {
            info!("Running summary command");
            debug!("Summary options: base={}", base);
            (summary_prompt(), get_branch_patches(base)?)
        }
    };

    let prompt = prompt.to_string();
    debug!("Using prompt: {}", prompt);

    let last_git_messages = get_last_git_messages(100)?;
    let last_git_messages_json = serde_json::to_string(&last_git_messages)?;
    let git_history_prompt = format!(
        "Follow the style of these git commit messages: {}",
        last_git_messages_json
    );

    let system_prompts: Vec<String> = vec![patch.to_string(), git_history_prompt];
    debug!("System prompt size: {} bytes", system_prompts[0].len());

    let tools = initialize_tools();

    let max_tokens = opts.max_tokens.unwrap_or(MAX_TOKENS);
    debug!("Max tokens: {}", max_tokens);

    let query_opts = Opts {
        max_tokens: Some(max_tokens),
        model: model,
        endpoint: opts.endpoint.clone(),
    };

    info!("Sending request to AI service");

    let mut messages: Vec<Message> = vec![];
    debug!("Using {} system prompts", system_prompts.len());
    for sp in system_prompts {
        messages.push(make_message("system", sp.clone()));
    }
    messages.push(make_message("user", prompt.clone()));

    let response = match post_request(messages, &tools, &query_opts) {
        Ok(resp) => resp,
        Err(e) => {
            return Err(e);
        }
    };

    let msg: String = match response.choices {
        Some(choices) if !choices.is_empty() => {
            debug!("Received {} choices from AI", choices.len());
            choices
                .into_iter()
                .map(|choice| choice.message.content)
                .collect()
        }
        _ => {
            return Err("No responses received".into());
        }
    };

    info!("AI response received, processing command");
    match opts.command {
        SubCommand::Fixup => {
            debug!("Processing fixup command");
            amend_commit(&msg)?;
        }
        SubCommand::Check => {
            debug!("Processing check command");
            check_commit(&msg)?;
        }
        SubCommand::Write {
            signoff,
            cached,
            interactive,
        } => {
            debug!("Processing write command");
            write_commit(&msg, signoff, cached, interactive)?;
        }
        SubCommand::Summary { .. } => {
            debug!("Processing summary command");
            println!("{}", msg);
        }
    };

    info!("Command completed successfully");
    Ok(())
}
