use std::env;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions, ReadDir};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;

use anyhow::{anyhow, Result};

const PROJECT_NAME: &'static str = "find_project";

struct SrcDir {
    path: PathBuf,
    search_depth: u8,
}

struct Project {
    path: PathBuf,
    name: String,
}

fn main() -> Result<()> {
    let mut config_file_path = PathBuf::from_str(env::var("XDG_CONFIG_HOME")?.as_str())?;
    let config_file_name = format!("{PROJECT_NAME}.conf");
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

    let mut fzf = Command::new("/usr/bin/fzf")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to run `fzf`.");

    if let Some(mut stdin) = fzf.stdin.take() {
        let project_strs =
            projects
                .iter()
                .filter_map(|p| p.path.to_str())
                .fold(String::new(), |mut acc, path| {
                    acc.push_str(path);
                    acc.push('\n');
                    acc
                });

        stdin
            .write_all(project_strs.as_bytes())
            .expect("Failed to write to `fzf` stdin");
    }

    let fzf_output = fzf.wait_with_output().unwrap();

    let selected_project_path = match fzf_output.status.code() {
        Some(0) => {
            let project_path = OsStr::from_bytes(fzf_output.stdout.as_slice());
            let project_path = PathBuf::from(project_path);

            project_path
        }
        Some(130) => return Err(anyhow!("You did not select project.")),
        Some(code) => return Err(anyhow!("fzf errored with code: {}.", code)),
        None => return Err(anyhow!("Nothing was returned by fzf.")),
    };

    println!("Selected: {:?}", selected_project_path);
    Ok(())
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
                    let name = String::from_utf8_lossy(entry.file_name().as_bytes()).into();
                    let project = Project {
                        path: entry.path(),
                        name,
                    };
                    res.push(project);
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
