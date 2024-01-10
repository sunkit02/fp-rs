use std::env;
use std::ffi::OsStr;
use std::fs::{self, ReadDir};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;

use anyhow::{anyhow, Result};

const PROJECT_NAME: &'static str = "find_project";

const FZF_BIN: &'static str = "/usr/bin/fzf";
const TMUX_BIN: &'static str = "/usr/bin/tmux";

/// A directory holding projects.
#[derive(Debug)]
struct SrcDir {
    /// The *full* path to the directory containing projects.
    path: PathBuf,
    /// The number of directories between the `path` to the actual projects
    search_depth: u8,
}

/// A project directory
#[derive(Debug)]
struct Project {
    /// The *full* path to the project root directory. (Including the directory name itself)
    inner: PathBuf,
}

impl Project {
    fn name(&self) -> Option<&str> {
        if let Some(s) = self.inner.file_name() {
            s.to_str()
        } else {
            None
        }
    }

    fn full_path(&self) -> &Path {
        self.inner.as_path()
    }
}

fn main() -> Result<()> {
    let mut config_file_path = PathBuf::from_str(env::var("XDG_CONFIG_HOME")?.as_str())?;
    let config_file_name = format!("{}.conf", PROJECT_NAME);
    config_file_path.push(PROJECT_NAME);
    config_file_path.push(config_file_name);

    let src_dirs = read_config_file(config_file_path)?;

    let projects = src_dirs
        .iter()
        .filter_map(|src_dir| {
            let dir = fs::read_dir(&src_dir.path).ok()?;
            Some(get_projects(dir, src_dir.search_depth).ok()?)
        })
        .flatten()
        .collect::<Vec<_>>();

    let list_sessions = Command::new(TMUX_BIN)
        .arg("list-sessions")
        .stdout(Stdio::piped())
        .spawn()?;

    let list_sessions_output = list_sessions.wait_with_output()?;

    let active_sessions = match list_sessions_output.status.code() {
        Some(0) => String::from_utf8_lossy(&list_sessions_output.stdout),
        Some(code) => return Err(anyhow!("tmux errored with code: {}.", code)),
        None => return Err(anyhow!("Nothing was returned by tmux.")),
    };

    // TODO: Add active sessions into fzf list
    let active_sessions = active_sessions
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(session_name, _)| session_name)
        .collect::<Vec<_>>();

    let mut fzf = Command::new(FZF_BIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to run `fzf`.");

    if let Some(mut stdin) = fzf.stdin.take() {
        let project_strs = projects.iter().filter_map(|p| p.full_path().to_str()).fold(
            String::new(),
            |mut acc, path| {
                acc.push_str(path);
                acc.push('\n');
                acc
            },
        );

        stdin
            .write_all(project_strs.as_bytes())
            .expect("Failed to write to `fzf` stdin");
    }

    let fzf_output = fzf.wait_with_output().unwrap();

    let selected_project_path = match fzf_output.status.code() {
        Some(0) => {
            let project_path = OsStr::from_bytes(fzf_output.stdout.as_slice());
            let project_path = project_path
                .to_str()
                .map(|s| s.trim())
                .ok_or_else(|| anyhow!("Failed to convert path from OsStr to str"))?;
            let project_path = PathBuf::from(project_path);

            project_path
        }
        Some(130) => return Err(anyhow!("You did not select project.")),
        Some(code) => return Err(anyhow!("fzf errored with code: {}.", code)),
        None => return Err(anyhow!("Nothing was returned by fzf.")),
    };

    let selected_project = Project {
        inner: selected_project_path,
    };

    println!("Selected: {:?}", selected_project);

    switch_to_project_in_tmux(&selected_project, &active_sessions)
}

fn read_config_file<P: AsRef<Path>>(path: P) -> Result<Vec<SrcDir>> {
    let mut src_dirs = fs::read_to_string(path.as_ref())?
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter_map(|(path, depth)| {
            let path = PathBuf::from_str(path).ok()?;
            let search_depth = depth.parse::<u8>().ok()?;
            Some(SrcDir { path, search_depth })
        })
        .collect::<Vec<_>>();

    let home_dir = env::var("HOME")?;
    let default_src_dir = SrcDir {
        path: PathBuf::from_str(format!("{home_dir}/src").as_str())?,
        search_depth: 2,
    };
    src_dirs.extend([default_src_dir]);

    Ok(src_dirs)
}

fn get_projects(mut src_dir: ReadDir, depth: u8) -> Result<Vec<Project>> {
    fn get_projects_recur(dir: &mut ReadDir, depth: u8, res: &mut Vec<Project>) -> Result<()> {
        if depth > 1 {
            while let Some(entry) = dir.next() {
                if let Ok(entry) = entry {
                    let metadata = entry.metadata()?;
                    if metadata.is_dir() {
                        get_projects_recur(&mut fs::read_dir(entry.path())?, depth - 1, res)?;
                    }
                }
            }

            return Ok(());
        }

        while let Some(entry) = dir.next() {
            if let Ok(entry) = entry {
                let metadata = entry.metadata()?;
                if metadata.is_dir() {
                    res.push(Project {
                        inner: entry.path(),
                    });
                }
            } else {
                continue;
            }
        }

        Ok(())
    }

    let mut projects = Vec::new();
    get_projects_recur(&mut src_dir, depth, &mut projects)?;

    Ok(projects)
}

fn switch_to_project_in_tmux(project: &Project, active_sessions: &[&str]) -> Result<()> {
    // Check if the user is currrently in a tmux session
    let in_tmux = env::var("TMUX").is_ok();
    let project_name = project
        .name()
        .ok_or_else(|| anyhow!("Failed to get project name."))?;
    let session_exists = active_sessions.contains(&project_name);

    let mut switch_session = Command::new(TMUX_BIN);

    if in_tmux {
        println!("In tmux");

        if session_exists {
            println!("Switching to session '{}'", project_name);

            // Command: "tmux switch -t {project.name}"
            switch_session
                .arg("switch") // switch session
                .arg("-t") // target session name
                .arg(project_name);
        } else {
            println!("Creating new session '{}'", project_name);

            // Command: "tmux new -c {project.path} -s {project.name} -d"
            let mut _create_session_as_daemon = Command::new(TMUX_BIN)
                .arg("new-session") // create new session
                .arg("-c") // change current working directory
                .arg(
                    project
                        .full_path()
                        .to_str()
                        .ok_or_else(|| anyhow!("Failed to convert full path to str."))?,
                )
                .arg("-s") // new session name
                .arg(project_name)
                .arg("-d") // initialize session in the background
                .spawn()?;

            // Command: "tmux attach -t {project.name}"
            switch_session
                .arg("switch") // switch session
                .arg("-t") // target session name
                .arg(project_name);
        }
    } else {
        println!("Not in tmux");

        if session_exists {
            println!("Attaching to session '{}'", project_name);

            // Command: "tmux attach -t {project.name}"
            switch_session
                .arg("attach") // attach to session
                .arg("-t") // target session name
                .arg(project_name);
        } else {
            println!("Creating new session '{}'", project_name);

            // Command: "tmux new -c {project.path} -s {project.name}"
            switch_session
                .arg("new-session") // create new session
                .arg("-c") // change current working directory
                .arg(
                    project
                        .full_path()
                        .to_str()
                        .ok_or_else(|| anyhow!("Failed to convert full path to str."))?,
                )
                .arg("-s") // new session name
                .arg(project_name);
        }
    }

    switch_session.spawn()?;

    Ok(())
}
