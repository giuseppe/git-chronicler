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
use codehawk::openai::{Opts, ToolsCollection, post_request};
use std::error::Error;
use std::io::Write;
use std::process::{Command, Stdio};
use string_builder::Builder;
use tempfile;

const DEFAULT_OPENAI_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const MODEL: &str = "google/gemini-2.5-pro-preview-03-25";
const MAX_TOKENS: u32 = 16384;

/// Creates a prompt for the AI model to improve an existing commit message.
fn inline_prompt() -> String {
    "Improve the git commit message for the patch and add any missing information you get from the code.  \
     Explain why a change is done, not what was changed.  Keep the first line below 52 columns and next ones under 80 columns.  \
     Return only the git commit message without any other information nor any delimiter.  \
     Leave unchanged any signed-off line or any other trailer:\n".to_owned()
}

/// Creates a prompt for the AI model to write a new commit message.
fn write_prompt() -> String {
    "Write the git commit message for the patch and add any information you get from the code.  \
     Explain why a change is done, not what was changed.  Keep the first line below 52 columns and next ones under 80 columns.  \
     Return only the git commit message without any other information nor any delimiter:\n".to_owned()
}

/// Creates a prompt for the AI model to check an existing commit message for errors.
fn check_prompt() -> String {
    "Report any mistake you see in the commit log message.  \
     If the input contains a significant error or discrepancy, the first line of the returned message must only contain the string ERROR and nothing more.  \
     Ignore the date and the author information, look only at the commit message.  \
     Explain carefully what changes you suggest:\n".to_owned()
}

/// Retrieves the last commit log message and patch using `git log -p -1`.
fn get_last_commit() -> Result<String, Box<dyn Error>> {
    let mut input = Command::new("git");
    input.arg("log").arg("-p").arg("-1");
    let output = input.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8(output.stderr)?;
        let err: Box<dyn Error> = stderr.into();
        return Err(err);
    }
    let r = String::from_utf8(output.stdout)?;
    Ok(r)
}

/// Retrieves the diff of changes using `git diff`.
fn get_diff(cached: bool) -> Result<String, Box<dyn Error>> {
    let mut git_cmd = Command::new("git");

    let mut input = git_cmd.arg("diff").arg("-U50");
    if cached {
        input = input.arg("--cached");
    }

    let output = input.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8(output.stderr)?;
        let err: Box<dyn Error> = stderr.into();
        return Err(err);
    }
    let r = String::from_utf8(output.stdout)?;
    Ok(r)
}

/// Creates a new commit using the provided commit message.
fn write_commit(
    commit_msg: &String,
    signoff: bool,
    cached: bool,
    interactive: bool,
) -> Result<(), Box<dyn Error>> {
    let mut git_cmd = Command::new("git");
    let mut cmd = git_cmd.arg("commit");
    if !cached {
        cmd = cmd.arg("-a");
    }
    if signoff {
        cmd = cmd.arg("-s");
    }

    if interactive {
        let tempfile = tempfile::NamedTempFile::new()?;
        let path = tempfile.path().to_str().ok_or("invalid temp file name")?;

        std::fs::write(tempfile.path(), commit_msg.as_bytes())?;

        cmd = cmd.args(["-F", path, "--edit"]);

        let mut child = cmd.spawn()?;
        child.wait()?;

        Ok(())
    } else {
        // Read from stdin if it is not running in interactive mode
        cmd = cmd.args(["-F", "-"]);

        let mut child = cmd.stdin(Stdio::piped()).spawn()?;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(commit_msg.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            let stderr = String::from_utf8(output.stderr)?;
            let err: Box<dyn Error> = stderr.into();
            return Err(err);
        }

        Ok(())
    }
}

/// Amends the last commit with the provided commit message.
fn amend_commit(commit_msg: &String) -> Result<(), Box<dyn Error>> {
    let mut child = Command::new("git")
        .args(["commit", "--amend", "-F", "-"])
        .stdin(Stdio::piped())
        .spawn()?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(commit_msg.as_bytes())?;
    }

    let output = child.wait_with_output()?;

    if !output.status.success() {
        let stderr = String::from_utf8(output.stderr)?;
        let err: Box<dyn Error> = stderr.into();
        return Err(err);
    }
    return Ok(());
}

/// Checks the AI's response for the 'check' command.
fn check_commit(msg: &String) -> Result<(), Box<dyn Error>> {
    if msg.starts_with("ERROR\n") {
        eprintln!("{}", &msg["ERROR\n".len()..].trim());
        return Err("wrong commit message".into());
    }
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
}

/// Main entry point for the git-chronicler application.
fn main() -> Result<(), Box<dyn Error>> {
    let opts = CliOpts::parse();

    let (prompt, patch) = match opts.command {
        SubCommand::Fixup => (inline_prompt(), get_last_commit()?),
        SubCommand::Check => (check_prompt(), get_last_commit()?),
        SubCommand::Write {
            signoff: _,
            cached,
            interactive: _,
        } => (write_prompt(), get_diff(cached)?),
    };

    let prompt = prompt.to_string();

    let system_prompts: Vec<String> = vec![patch.to_string()];

    let tools: ToolsCollection = ToolsCollection::new();

    let query_opts = Opts {
        max_tokens: Some(opts.max_tokens.unwrap_or_else(|| MAX_TOKENS)),
        model: opts.model.unwrap_or_else(|| MODEL.to_string()),
        endpoint: opts.endpoint.clone(),
    };

    let response = post_request(&prompt, Some(system_prompts), None, &tools, &query_opts)?;

    let mut builder = Builder::default();
    if let Some(choices) = response.choices {
        for choice in choices {
            builder.append(choice.message.content);
        }
    }
    let msg = builder.string()?;

    match opts.command {
        SubCommand::Fixup => {
            amend_commit(&msg)?;
        }
        SubCommand::Check => {
            check_commit(&msg)?;
        }
        SubCommand::Write {
            signoff,
            cached,
            interactive,
        } => {
            write_commit(&msg, signoff, cached, interactive)?;
        }
    };

    Ok(())
}
