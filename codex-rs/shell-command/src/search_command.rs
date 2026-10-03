//! Recognize literal search workflows that should have a bounded runtime.
//!
//! This is a runtime-budget heuristic, not an authorization or sandbox decision.
//! Unknown syntax and mixed workflows keep their existing execution lifetime.

use crate::bash::extract_bash_command;
use crate::bash::parse_shell_script_into_commands;
use crate::powershell::UTF8_OUTPUT_PREFIX;
use crate::powershell::extract_powershell_command;
use crate::powershell::parse_powershell_script_into_plain_commands;

/// Whether every literal command is a search or a read-only companion to one.
///
/// Parse shell syntax instead of looking for search names in arbitrary text: a
/// quoted pattern, an echo, or a search followed by a server must not impose a
/// deadline on the server. PowerShell is parsed without launching a subprocess.
pub fn is_search_only_command(command: &[String]) -> bool {
    // A remote Windows executor can be driven by a Unix host. Normalize only
    // the executable used for shell recognition, preserving every script byte.
    let mut normalized_command = command.to_vec();
    if let Some(program) = normalized_command.first_mut() {
        *program = program
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(program)
            .to_ascii_lowercase();
    }
    let command = normalized_command.as_slice();
    let commands = if let Some((_, script)) = extract_powershell_command(command) {
        let script = script.trim_start();
        parse_powershell_script_into_plain_commands(
            script.strip_prefix(UTF8_OUTPUT_PREFIX).unwrap_or(script),
        )
    } else if let Some((_, script)) = extract_bash_command(command) {
        parse_shell_script_into_commands(script)
    } else {
        Some(vec![command.to_vec()])
    };
    let Some(commands) = commands else {
        return false;
    };

    let mut has_search = false;
    for command in commands {
        let Some(program) = command.first() else {
            return false;
        };
        let program = program.rsplit(['/', '\\']).next().unwrap_or(program);
        let program = program.to_ascii_lowercase();
        let program = program.strip_suffix(".exe").unwrap_or(&program);
        match program {
            "rg" | "grep" | "egrep" | "fgrep" | "findstr" | "select-string" => {
                has_search = true;
            }
            "git" if command.get(1).is_some_and(|arg| arg == "grep") => {
                has_search = true;
            }
            "git" if command.get(1).is_some_and(|arg| arg == "ls-files") => {}
            "cat" | "head" | "wc" | "get-content" | "select-object" | "measure-object"
            | "out-string" => {
                // Get-Content -Wait is an intentional live stream, not a finite search.
                if matches!(program, "get-content" | "cat")
                    && command.iter().any(|arg| {
                        let arg = arg.to_ascii_lowercase();
                        let arg = arg.split(':').next().unwrap_or(&arg);
                        arg.starts_with("-w") && "-wait".starts_with(arg)
                    })
                {
                    return false;
                }
            }
            _ => return false,
        }
    }
    has_search
}

#[cfg(test)]
#[path = "search_command_tests.rs"]
mod tests;
