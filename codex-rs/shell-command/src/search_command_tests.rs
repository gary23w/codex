use super::is_search_only_command;
use crate::powershell::UTF8_OUTPUT_PREFIX;

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

#[test]
fn recognizes_direct_search_executables_and_git_grep() {
    for command in [
        argv(&["rg", "-l", "-F", "missing", ".local"]),
        argv(&[r"C:\tools\rg.exe", "--files"]),
        argv(&["/usr/bin/grep", "-R", "missing", "."]),
        argv(&["findstr.exe", "/s", "missing", "*.txt"]),
        argv(&["git", "grep", "missing", "--", "src"]),
    ] {
        assert!(is_search_only_command(&command), "{command:?}");
    }
}

#[test]
fn recognizes_powershell_searches_with_output_limits_and_followup_reads() {
    for script in [
        "rg --no-ignore -l -F 'missing' .local --glob '*.json' --glob '!**/node_modules/**' | Select-Object -First 12",
        "rg -l -F 'missing' .local; Get-Content baseline.json | Select-Object -Skip 20 -First 15",
        "Select-String -LiteralPath source.txt -Pattern 'rg; npm run dev'",
        "Get-Content source.txt | Select-String missing",
    ] {
        let command = argv(&["pwsh", "-NoProfile", "-Command", script]);
        assert!(is_search_only_command(&command), "{script}");
        let prefixed = format!("{UTF8_OUTPUT_PREFIX}{script}");
        let command = argv(&["powershell.exe", "-Command", &prefixed]);
        assert!(is_search_only_command(&command), "{prefixed}");
    }
}

#[test]
fn recognizes_literal_posix_search_pipelines() {
    for script in [
        "rg -n 'foo|bar' src | head -100",
        "git grep missing -- src | wc -l",
        "rg missing src; cat README.md",
    ] {
        assert!(is_search_only_command(&argv(&["bash", "-lc", script])));
    }
}

#[test]
fn preserves_nonsearch_and_mixed_workflow_lifetimes() {
    for command in [
        vec![],
        argv(&["node", "server.js"]),
        argv(&["git", "ls-files", "src"]),
        argv(&["echo", "rg missing ."]),
        argv(&["bash", "-lc", "echo 'rg missing .'"]),
        argv(&["bash", "-lc", "rg missing src; npm run dev"]),
        argv(&["bash", "-lc", "tail -f app.log | grep error"]),
        argv(&["bash", "-lc", "rg missing . &"]),
        argv(&["pwsh", "-Command", "Write-Output 'rg missing .'"]),
        argv(&["pwsh", "-Command", "rg missing src; node server.js"]),
        argv(&[
            "pwsh",
            "-Command",
            "Get-Content app.log -Wait | Select-String error",
        ]),
        argv(&["pwsh", "-Command", "& $searchTool missing ."]),
        argv(&[
            "pwsh",
            "-Command",
            "cat app.log -Wait | Select-String error",
        ]),
        argv(&[
            "pwsh",
            "-Command",
            "Get-Content app.log -Wait:$true | Select-String error",
        ]),
        argv(&[
            "pwsh",
            "-Command",
            "Get-Content app.log -W | Select-String error",
        ]),
        argv(&["pwsh", "-Command", "& 'C:\\tools\\rg.exe' -n missing src"]),
        argv(&[
            "pwsh",
            "-Command",
            "rg missing src | ForEach-Object { Remove-Item $_ }",
        ]),
    ] {
        assert!(!is_search_only_command(&command), "{command:?}");
    }
}

#[test]
fn recognizes_windows_shell_paths_independently_of_the_host_os() {
    let command = argv(&[
        r"C:\Windows\System32\WindowsPowerShell\v1.0\PowerShell.EXE",
        "-NoProfile",
        "-Command",
        "rg missing .local | Select-Object -First 12",
    ]);
    assert!(is_search_only_command(&command));
}

#[test]
fn does_not_classify_dynamic_scripts_as_finite_searches() {
    for command in [
        argv(&["pwsh", "-Command", "while ($true) { rg missing src }"]),
        argv(&["pwsh", "-EncodedCommand", "cgBnACAAbQBpAHMAcwBpAG4AZwA="]),
        argv(&["bash", "-lc", "while true; do rg missing src; done"]),
        argv(&["bash", "-lc", "rg \"$(node generate-pattern.js)\" src"]),
        argv(&["bash", "-lc", "rg missing src | sh"]),
    ] {
        assert!(!is_search_only_command(&command), "{command:?}");
    }
}
