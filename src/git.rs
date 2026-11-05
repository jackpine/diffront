use std::path::PathBuf;
use std::process::Command;

/// A diff the user can pick without typing its arguments.
#[derive(Clone)]
pub struct BaseOption {
    pub label: String,
    pub args: Vec<String>,
}

impl BaseOption {
    fn new(label: impl Into<String>, args: &[&str]) -> Self {
        Self {
            label: label.into(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
        }
    }

    fn since_branch(label: impl Into<String>, branch: &str) -> Self {
        Self::new(label, &["--merge-base", branch])
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub sha: String,
    pub short: String,
    pub subject: String,
    pub author: String,
    pub when: String,
    /// False for a commit kept only because it has comments after leaving the range.
    pub in_range: bool,
}

/// The git repository containing the current directory.
#[derive(Clone)]
pub struct Repo {
    root: PathBuf,
    working_dir: PathBuf,
}

impl Repo {
    pub fn discover() -> Result<Self, String> {
        let root = Self::run(
            Command::new("git").args(["rev-parse", "--show-toplevel"]),
            &[0],
        )?;
        let working_dir = std::env::current_dir()
            .map_err(|err| format!("failed to read the current directory: {err}"))?;
        Ok(Self {
            root: PathBuf::from(root.trim_end()),
            working_dir,
        })
    }

    #[cfg(test)]
    pub fn at(path: &std::path::Path) -> Self {
        Self {
            root: path.to_path_buf(),
            working_dir: path.to_path_buf(),
        }
    }

    /// Names the diff that `patch` produces for these arguments.
    pub fn describe(args: &[String]) -> String {
        if args.is_empty() {
            "git diff HEAD + untracked files".to_string()
        } else {
            format!("git diff {}", args.join(" "))
        }
    }

    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string())
    }

    /// Commits being reviewed under `args`, or recent history if that range is empty.
    pub fn commits(&self, args: &[String]) -> Result<Vec<Commit>, String> {
        const RECENT: &str = "HEAD";
        let revisions = Self::revisions(args);
        let range = match revisions.as_slice() {
            [] => self
                .default_branch()
                .map(|branch| format!("{branch}..HEAD")),
            [range] if range.contains("..") => Some(range.replace("...", "..")),
            [base] => Some(format!("{base}..HEAD")),
            [from, to, ..] => Some(format!("{from}..{to}")),
        };
        let commits = match range {
            Some(range) => self.log(&range)?,
            None => Vec::new(),
        };
        if commits.is_empty() && revisions.is_empty() {
            self.log(RECENT)
        } else {
            Ok(commits)
        }
    }

    fn revisions(args: &[String]) -> Vec<&str> {
        args.iter()
            .take_while(|arg| *arg != "--")
            .filter(|arg| !arg.starts_with('-'))
            .map(String::as_str)
            .collect()
    }

    fn log(&self, range: &str) -> Result<Vec<Commit>, String> {
        let log = Self::run(
            Command::new("git").current_dir(&self.root).args([
                "log",
                "--no-color",
                "--max-count=200",
                "--reverse",
                "--format=%H%x00%h%x00%s%x00%an%x00%ar",
                range,
            ]),
            &[0],
        )?;
        Ok(log.lines().filter_map(Self::parse_log_line).collect())
    }

    fn parse_log_line(line: &str) -> Option<Commit> {
        let mut fields = line.split('\0').map(str::to_string);
        Some(Commit {
            sha: fields.next()?,
            short: fields.next()?,
            subject: fields.next()?,
            author: fields.next()?,
            when: fields.next()?,
            in_range: true,
        })
    }

    /// Unified diff of one commit, against its first parent.
    pub fn commit_patch(&self, sha: &str) -> Result<String, String> {
        Self::run(
            Command::new("git").current_dir(&self.root).args([
                "-c",
                "core.quotepath=off",
                "show",
                "--format=",
                "--patch",
                "--first-parent",
                "--no-color",
                "--no-ext-diff",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                sha,
            ]),
            &[0],
        )
    }

    /// Common review bases, with the default branch first.
    pub fn base_options(&self) -> Vec<BaseOption> {
        const MAX_RECENT_BRANCHES: usize = 5;
        let default_branch = self.default_branch();
        let current_branch = self.git_output(&["symbolic-ref", "--quiet", "--short", "HEAD"]);
        let mut options = Vec::new();
        if let Some(branch) = &default_branch {
            options.push(BaseOption::since_branch(
                format!("{branch} (default branch)"),
                branch,
            ));
        }
        options.push(BaseOption::new("Uncommitted changes", &[]));
        options.push(BaseOption::new("Staged changes", &["--cached"]));
        if self.ref_exists("HEAD~1") {
            options.push(BaseOption::new("Last commit", &["HEAD~1", "HEAD"]));
        }
        let recent = self
            .git_output(&[
                "for-each-ref",
                "--sort=-committerdate",
                "--format=%(refname:short)",
                "refs/heads",
            ])
            .unwrap_or_default();
        let default_name = default_branch
            .as_deref()
            .map(|branch| branch.strip_prefix("origin/").unwrap_or(branch));
        options.extend(
            recent
                .lines()
                .filter(|branch| Some(*branch) != current_branch.as_deref())
                .filter(|branch| Some(*branch) != default_name)
                .take(MAX_RECENT_BRANCHES)
                .map(|branch| BaseOption::since_branch(branch, branch)),
        );
        options
    }

    fn default_branch(&self) -> Option<String> {
        self.git_output(&[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ])
        .or_else(|| {
            let configured = self.git_output(&["config", "init.defaultBranch"]);
            configured
                .into_iter()
                .chain(["main".to_string(), "master".to_string()])
                .find(|branch| self.ref_exists(&format!("refs/heads/{branch}")))
        })
    }

    fn ref_exists(&self, reference: &str) -> bool {
        self.git_output(&["rev-parse", "--verify", "--quiet", reference])
            .is_some()
    }

    fn git_output(&self, args: &[&str]) -> Option<String> {
        let output =
            Self::run(Command::new("git").current_dir(&self.root).args(args), &[0]).ok()?;
        let output = output.trim_end();
        (!output.is_empty()).then(|| output.to_string())
    }

    /// Unified diff for `git diff <args>`, or all uncommitted work when `args` is empty.
    pub fn patch(&self, args: &[String]) -> Result<String, String> {
        if args.is_empty() {
            self.uncommitted_patch()
        } else {
            Self::run(
                Self::diff_command()
                    .current_dir(&self.working_dir)
                    .args(args),
                &[0],
            )
        }
    }

    fn uncommitted_patch(&self) -> Result<String, String> {
        let base = self.base_tree()?;
        let mut patch = Self::run(Self::diff_command().current_dir(&self.root).arg(base), &[0])?;
        for path in self.untracked_files()? {
            // `--no-index` exits 1 when the files differ.
            patch += &Self::run(
                Self::diff_command().current_dir(&self.root).args([
                    "--no-index",
                    "--",
                    "/dev/null",
                    &path,
                ]),
                &[0, 1],
            )?;
        }
        Ok(patch)
    }

    fn base_tree(&self) -> Result<String, String> {
        let has_commits = Command::new("git")
            .current_dir(&self.root)
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .output()
            .map_err(|err| format!("failed to run git: {err}"))?
            .status
            .success();
        if has_commits {
            return Ok("HEAD".to_string());
        }
        let empty_tree = Self::run(
            Command::new("git").current_dir(&self.root).args([
                "hash-object",
                "-t",
                "tree",
                "/dev/null",
            ]),
            &[0],
        )?;
        Ok(empty_tree.trim_end().to_string())
    }

    fn untracked_files(&self) -> Result<Vec<String>, String> {
        let listing = Self::run(
            Command::new("git").current_dir(&self.root).args([
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
            ]),
            &[0],
        )?;
        // A trailing slash marks a nested repository, which has no diffable content.
        Ok(listing
            .split('\0')
            .filter(|path| !path.is_empty() && !path.ends_with('/'))
            .map(str::to_string)
            .collect())
    }

    /// A `git diff` whose output format doesn't depend on the user's git config.
    fn diff_command() -> Command {
        let mut command = Command::new("git");
        command.args([
            "-c",
            "core.quotepath=off",
            "diff",
            "--patch",
            "--no-color",
            "--no-ext-diff",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ]);
        command
    }

    fn run(command: &mut Command, ok_codes: &[i32]) -> Result<String, String> {
        let output = command
            .output()
            .map_err(|err| format!("failed to run git: {err}"))?;
        if output
            .status
            .code()
            .is_some_and(|code| ok_codes.contains(&code))
        {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRepo(PathBuf);

    impl TestRepo {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("diffront-git-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            let repo = Self(path);
            repo.git(&["init", "-q", "-b", "main"]);
            repo.commit("first");
            repo
        }

        fn git(&self, args: &[&str]) {
            let status = Command::new("git")
                .current_dir(&self.0)
                .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }

        fn commit(&self, message: &str) {
            self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        }

        fn subjects(&self, args: &[&str]) -> Vec<String> {
            let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
            Repo::at(&self.0)
                .commits(&args)
                .unwrap()
                .into_iter()
                .map(|commit| commit.subject)
                .collect()
        }

        fn labels(&self) -> Vec<String> {
            Repo::at(&self.0)
                .base_options()
                .into_iter()
                .map(|option| option.label)
                .collect()
        }
    }

    impl Drop for TestRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn default_branch_comes_first_and_current_branch_is_skipped() {
        let repo = TestRepo::new("options");
        repo.git(&["branch", "other"]);
        repo.git(&["checkout", "-q", "-b", "feature"]);
        repo.commit("second");
        assert_eq!(
            repo.labels(),
            vec![
                "main (default branch)",
                "Uncommitted changes",
                "Staged changes",
                "Last commit",
                "other",
            ]
        );
    }

    #[test]
    fn origin_head_decides_the_default_branch() {
        let repo = TestRepo::new("origin");
        repo.git(&["update-ref", "refs/remotes/origin/develop", "HEAD"]);
        repo.git(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/develop",
        ]);
        assert_eq!(repo.labels()[0], "origin/develop (default branch)");
    }

    #[test]
    fn single_commit_repo_has_no_last_commit_option() {
        let repo = TestRepo::new("single");
        assert!(!repo.labels().contains(&"Last commit".to_string()));
    }

    #[test]
    fn commit_range_follows_the_diff_arguments() {
        let repo = TestRepo::new("range");
        repo.git(&["checkout", "-q", "-b", "feature"]);
        repo.commit("second");
        repo.commit("third");
        assert_eq!(repo.subjects(&[]), vec!["second", "third"]);
        assert_eq!(
            repo.subjects(&["--merge-base", "main"]),
            vec!["second", "third"]
        );
        assert_eq!(repo.subjects(&["main...HEAD"]), vec!["second", "third"]);
        assert_eq!(repo.subjects(&["HEAD~1"]), vec!["third"]);
        assert_eq!(repo.subjects(&["HEAD~2", "HEAD~1"]), vec!["second"]);
        assert_eq!(repo.subjects(&["HEAD~1", "--", "src"]), vec!["third"]);
    }

    #[test]
    fn default_branch_with_nothing_ahead_shows_recent_history() {
        let repo = TestRepo::new("recent");
        repo.commit("second");
        assert_eq!(repo.subjects(&[]), vec!["first", "second"]);
    }

    #[test]
    fn root_commit_has_a_patch() {
        let repo = TestRepo::new("root");
        std::fs::write(repo.0.join("a.txt"), "one\n").unwrap();
        repo.git(&["add", "-A"]);
        repo.commit("add file");
        let root = Repo::at(&repo.0);
        let sha = root.commits(&[]).unwrap().pop().unwrap().sha;
        assert!(root.commit_patch(&sha).unwrap().contains("+one"));
    }
}
