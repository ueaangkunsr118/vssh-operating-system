use std::env;
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Write};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use anyhow::Result;
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{chdir, execvp, fork, ForkResult};

fn main() -> Result<()> {
    loop {
        let cwd = env::current_dir()?;
        print!("{}$ ", cwd.display());
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        if input == "exit" {
            break;
        }

        if input.starts_with("cd ") {
            let dir = input[3..].trim();
            if let Err(e) = chdir(Path::new(dir)) {
                eprintln!("cd error: {}", e);
            }
            continue;
        }

        let (input, background) = if input.ends_with('&') {
            (input.trim_end_matches('&').trim(), true)
        } else {
            (input, false)
        };

        if let Err(e) = execute_line(input, background) {
            eprintln!("Error: {}", e);
        }
    }
    Ok(())
}

fn execute_line(line: &str, background: bool) -> Result<()> {
    let mut segments: Vec<&str> = line.split('|').map(str::trim).collect();
    if segments.is_empty() {
        return Ok(());
    }

    let mut input_file = None;
    if segments[0].contains('<') {
        let parts: Vec<&str> = segments[0].split('<').map(str::trim).collect();
        segments[0] = parts[0];
        input_file = Some(parts[1]);
    }

    let mut output_file = None;
    let last = segments.len() - 1;
    if segments[last].contains('>') {
        let parts: Vec<&str> = segments[last].split('>').map(str::trim).collect();
        segments[last] = parts[0];
        output_file = Some(parts[1]);
    }

    let mut pipes = Vec::new();
    for _ in 0..segments.len() - 1 {
        let mut fds = [0, 0];
        unsafe {
            if libc::pipe(fds.as_mut_ptr()) == -1 {
                return Err(anyhow::anyhow!("Failed to create pipe"));
            }
            pipes.push((fds[0], fds[1]));
        }
    }

    let mut child_pids = Vec::new();

    for i in 0..segments.len() {
        let args = externalize(segments[i]);
        if args.is_empty() {
            continue;
        }

        match unsafe { fork()? } {
            ForkResult::Child => {
                if i == 0 {
                    if let Some(file) = input_file {
                        let infile = File::open(file)?;
                        unsafe {
                            libc::dup2(infile.as_raw_fd(), libc::STDIN_FILENO);
                        }
                    }
                }

                if i == segments.len() - 1 {
                    if let Some(file) = output_file {
                        let outfile = File::create(file)?;
                        unsafe {
                            libc::dup2(outfile.as_raw_fd(), libc::STDOUT_FILENO);
                        }
                    }
                }

                if i > 0 {
                    unsafe {
                        libc::dup2(pipes[i - 1].0, libc::STDIN_FILENO);
                    }
                }

                if i < segments.len() - 1 {
                    unsafe {
                        libc::dup2(pipes[i].1, libc::STDOUT_FILENO);
                    }
                }

                for &(read_fd, write_fd) in &pipes {
                    unsafe {
                        libc::close(read_fd);
                        libc::close(write_fd);
                    }
                }

                execvp(&args[0], &args)?;
                std::process::exit(1);
            }
            ForkResult::Parent { child } => {
                child_pids.push(child);
                if background {
                    println!("Started background process with PID: {}", child);
                }
            }
        }
    }

    for &(read_fd, write_fd) in &pipes {
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
    }

    if !background {
        for pid in child_pids {
            let status = waitpid(pid, None)?;
            match status {
                WaitStatus::Exited(pid, status) => {
                    println!("Process {} exited with status {}", pid, status);
                }
                WaitStatus::Signaled(pid, signal, core_dumped) => {
                    println!(
                        "Process {} was killed by signal {:?}, core dumped: {}",
                        pid, signal, core_dumped
                    );
                }
                WaitStatus::Continued(pid) => {
                    println!("Process {} continued", pid);
                }
                WaitStatus::Stopped(pid, signal) => {
                    println!("Process {} stopped by signal {:?}", pid, signal);
                }
                WaitStatus::StillAlive => {
                    println!("No state changes to report");
                }
                _ => {}
            }
        }
    }

    Ok(())
}

fn externalize(command: &str) -> Vec<CString> {
    command
        .split_whitespace()
        .map(|s| CString::new(s).unwrap())
        .collect()
}