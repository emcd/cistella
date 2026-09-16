//! Conduct CLI surface: session-directory rename, hidden alias, mounts.

use clap::Parser;

use cistella::cli::{Cli, Command};

fn conduct(args: &[&str]) -> Command {
    let mut full = vec!["cistella", "conduct"];
    full.extend(args);
    match Cli::try_parse_from(full).unwrap().command {
        cmd @ Command::Conduct { .. } => cmd,
        other => panic!("expected conduct, got {other:?}"),
    }
}

#[test]
fn session_directory_replaces_directory() {
    match conduct(&["--profile", "x", "--session-directory", "/repo:/repo"]) {
        Command::Conduct {
            session_directory,
            mounts,
            ..
        } => {
            assert_eq!(session_directory.as_deref(), Some("/repo:/repo"));
            assert!(mounts.is_empty());
        }
        other => panic!("expected conduct, got {other:?}"),
    }
}

#[test]
fn cwd_alias_reaches_session_directory() {
    match conduct(&["--profile", "x", "--cwd", "/repo"]) {
        Command::Conduct {
            session_directory, ..
        } => assert_eq!(session_directory.as_deref(), Some("/repo")),
        other => panic!("expected conduct, got {other:?}"),
    }
}

#[test]
fn mount_flag_is_repeatable() {
    match conduct(&[
        "--profile",
        "x",
        "--mount",
        "/a:/a:ro",
        "--mount",
        "/b:/b:rw",
    ]) {
        Command::Conduct { mounts, .. } => {
            assert_eq!(mounts, vec!["/a:/a:ro".to_string(), "/b:/b:rw".to_string()]);
        }
        other => panic!("expected conduct, got {other:?}"),
    }
}

#[test]
fn project_name_flag_reaches_conduct() {
    match conduct(&["--profile", "x", "--project-name", "qa"]) {
        Command::Conduct { project_name, .. } => {
            assert_eq!(project_name.as_deref(), Some("qa"));
        }
        other => panic!("expected conduct, got {other:?}"),
    }
}

#[test]
fn selectors_keep_directory() {
    match Cli::try_parse_from(["cistella", "enter", "--directory", "/repo", "--", "sh"])
        .unwrap()
        .command
    {
        Command::Enter { directory, .. } => {
            assert_eq!(directory.as_deref(), Some("/repo"));
        }
        other => panic!("expected enter, got {other:?}"),
    }
}
