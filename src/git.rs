use std::path::PathBuf;
use std::process::Command;

/// The git repository containing the current directory.
pub struct Repo {
    root: PathBuf,
}

impl Repo {
    pub fn discover() -> Result<Self, String> {
        let root = Self::run(
            Command::new("git").args(["rev-parse", "--show-toplevel"]),
            &[0],
        )?;
        Ok(Self {
            root: PathBuf::from(root.trim_end()),
        })
    }

    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string())
    }

    /// Unified diff for `git diff <args>`, or all uncommitted work when `args` is empty.
    pub fn patch(&self, args: &[String]) -> Result<String, String> {
        if args.is_empty() {
            self.uncommitted_patch()
        } else {
            Self::run(Self::diff_command().args(args), &[0])
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
