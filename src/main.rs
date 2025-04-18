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
use dirs;
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;
use string_builder::Builder;

const OPEN_ROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
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

/// Reads the OpenRouter API key from the user's home directory.
fn read_api_key() -> Result<String, Box<dyn Error>> {
    let home_dir = dirs::home_dir().ok_or("Could not find home directory")?;
    let key_path = home_dir.join(".openrouter").join("key");

    let mut file = File::open(&key_path)
        .map_err(|e| format!("Failed to open key file at {:?}: {}", key_path, e))?;

    let mut api_key = String::new();
    file.read_to_string(&mut api_key)?;

    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        return Err("API key file is empty".into());
    }

    Ok(api_key)
}

#[derive(Serialize)]
struct OpenRouterRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<Message>,
}

#[derive(Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct OpenRouterResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
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

    let mut input = git_cmd.arg("diff");
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
fn write_commit(commit_msg: &String, signoff: bool, cached: bool) -> Result<(), Box<dyn Error>> {
    let mut git_cmd = Command::new("git");
    let mut cmd = git_cmd.args(["commit", "-F", "-"]);
    if !cached {
        cmd = cmd.arg("-a");
    }
    if signoff {
        cmd = cmd.arg("-s");
    }
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
    return Ok(());
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
struct Opts {
    /// Maximum number of tokens to generate
    #[clap(short, long)]
    max_tokens: Option<u32>,
    #[clap(long)]
    /// Override the model to use
    model: Option<String>,
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
    },
    /// Fixup the current commit message inline
    Fixup,
    /// Check if the commit message describes correctly the patch
    Check,
}

/// Main entry point for the git-chronicler application.
fn main() -> Result<(), Box<dyn Error>> {
    let opts = Opts::parse();

    let api_key = read_api_key()?;

    let bearer_auth = format!("Bearer {}", &api_key);

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(AUTHORIZATION, HeaderValue::from_str(&bearer_auth)?);

    let (prompt, patch) = match opts.command {
        SubCommand::Fixup => (inline_prompt(), get_last_commit()?),
        SubCommand::Check => (check_prompt(), get_last_commit()?),
        SubCommand::Write { signoff: _, cached } => (write_prompt(), get_diff(cached)?),
    };

    let request_body = OpenRouterRequest {
        model: opts.model.unwrap_or_else(|| MODEL.to_string()),
        max_tokens: opts.max_tokens.unwrap_or_else(|| MAX_TOKENS),
        messages: vec![
            Message {
                role: "system".to_string(),
                content: patch.to_string(),
            },
            Message {
                role: "user".to_string(),
                content: prompt.to_string(),
            },
        ],
    };

    let client = Client::new();
    let response = client
        .post(OPEN_ROUTER_URL)
        .timeout(Duration::from_secs(1000))
        .headers(headers)
        .json(&request_body)
        .send()?;

    if !response.status().is_success() {
        eprintln!(
            "Got Error Code: {}: {}",
            response.status(),
            response.text()?
        );
    } else {
        let response: OpenRouterResponse = response.json()?;

        let mut builder = Builder::default();
        for choice in response.choices {
            builder.append(choice.message.content);
        }
        let msg = builder.string()?;

        match opts.command {
            SubCommand::Fixup => {
                amend_commit(&msg)?;
            }
            SubCommand::Check => {
                check_commit(&msg)?;
            }
            SubCommand::Write { signoff, cached } => {
                write_commit(&msg, signoff, cached)?;
            }
        };
    }

    Ok(())
}
