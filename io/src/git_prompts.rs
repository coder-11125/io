//! `/review`, `/commit` and `/pr`: slash commands that expand into a prompt
//! for the agent. They run as ordinary turns, so every command they lead to
//! goes through the normal permission checks.

/// The prompt a git slash command stands for, or `None` if `input` is not one.
pub fn expand(input: &str) -> Option<String> {
    let (command, arg) = match input.split_once(char::is_whitespace) {
        Some((c, a)) => (c, a.trim()),
        None => (input, ""),
    };
    match command {
        "/review" => Some(review_prompt(arg)),
        "/commit" => Some(commit_prompt(arg)),
        "/pr" => Some(pr_prompt(arg)),
        _ => None,
    }
}

fn review_prompt(arg: &str) -> String {
    let target = if arg.is_empty() {
        "the uncommitted changes in this repository: run `git status` and `git diff HEAD` (and \
         look at untracked files that are part of the change)"
            .to_string()
    } else if arg.chars().all(|c| c.is_ascii_digit()) {
        format!("pull request #{arg}: run `gh pr view {arg}` and `gh pr diff {arg}`")
    } else {
        format!("the changes in `{arg}`: run `git diff {arg}` (a branch or range against the current branch)")
    };
    format!(
        "Review {target}.\n\n\
         Read the changed code in context (open the surrounding files, not just the diff) and \
         report real problems, most serious first: bugs and wrong behaviour, missed edge cases, \
         security issues, broken or missing tests, and API or compatibility breaks. For each, give \
         file:line, what goes wrong and a concrete fix. Skip style nits unless they hide a bug. \
         If the change looks correct, say so plainly. Do not modify any files."
    )
}

fn commit_prompt(arg: &str) -> String {
    let hint = if arg.is_empty() {
        String::new()
    } else {
        format!("\nThe user adds: {arg}\n")
    };
    format!(
        "Create a git commit for the current changes.\n\n\
         1. Run `git status`, `git diff HEAD` and `git log -n 15 --format=%B---` to see what \
         changed and how this repository words its commits.\n\
         2. Match that style exactly — subject line shape, tense, body, trailers. Do not use \
         Conventional Commits unless the log does.\n\
         3. Stage only the files that belong to this change (never secrets, build output or \
         unrelated edits), then commit.\n\
         4. Do not push, amend, or rewrite history.\n\
         5. Show the resulting commit.{hint}"
    )
}

fn pr_prompt(arg: &str) -> String {
    let hint = if arg.is_empty() {
        String::new()
    } else {
        format!("\nThe user adds: {arg}\n")
    };
    format!(
        "Open a pull request for the current branch.\n\n\
         1. Run `git status`, `git log` and `git diff <base>...HEAD` to understand the whole \
         branch (the base is the repository's default branch). Commit nothing the user hasn't \
         asked for; if there are uncommitted changes, say so and stop.\n\
         2. Look for a pull request template (`.github/pull_request_template.md`, \
         `.github/PULL_REQUEST_TEMPLATE*`, `docs/`) and follow its structure if there is one.\n\
         3. Write a title and description about what changed and why, in the repository's \
         style.\n\
         4. Push the branch with `git push -u origin <branch>` and create the PR with \
         `gh pr create`, then print its URL.{hint}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_git_commands_expand() {
        assert!(expand("/review").is_some());
        assert!(expand("/commit fix the typo").is_some());
        assert!(expand("/pr").is_some());
        assert!(expand("/reviewer").is_none());
        assert!(expand("review").is_none());
        assert!(expand("/help").is_none());
    }

    #[test]
    fn review_targets_changes_a_pr_or_a_branch() {
        assert!(expand("/review").unwrap().contains("git diff HEAD"));
        let pr = expand("/review 42").unwrap();
        assert!(pr.contains("gh pr diff 42") && pr.contains("pull request #42"));
        assert!(expand("/review main..feature")
            .unwrap()
            .contains("git diff main..feature"));
        assert!(expand("/review")
            .unwrap()
            .contains("Do not modify any files"));
    }

    #[test]
    fn commit_follows_the_repos_own_style_and_never_pushes() {
        let p = expand("/commit only the parser").unwrap();
        assert!(p.contains("git log -n 15"));
        assert!(p.contains("Do not use Conventional Commits unless the log does"));
        assert!(p.contains("Do not push"));
        assert!(p.contains("The user adds: only the parser"));
        assert!(!expand("/commit").unwrap().contains("The user adds"));
    }

    #[test]
    fn pr_looks_for_a_template() {
        let p = expand("/pr draft").unwrap();
        assert!(p.contains("pull_request_template.md") && p.contains("gh pr create"));
        assert!(p.contains("The user adds: draft"));
    }
}
