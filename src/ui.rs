//! Action-menu backend selection is independent of content providers.
//! Sidebar and switchers always use their shared list renderer.
use crate::{paths::Paths, server::ServerIdentity};
use clap::ValueEnum;
use std::{fs, process::Command};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum Mode {
    Old,
    #[default]
    New,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "old" => Some(Self::Old),
            "new" => Some(Self::New),
            _ => None,
        }
    }

    pub fn current() -> Result<Self> {
        if let Ok(server) = ServerIdentity::discover() {
            let output = Command::new("tmux")
                .args(server.tmux_args())
                .args(["show-option", "-gqv", "@workbench-ui"])
                .output()?;
            if output.status.success()
                && let Some(mode) = Self::parse(&String::from_utf8_lossy(&output.stdout))
            {
                return Ok(mode);
            }
        }
        let saved = Paths::discover()?.config_dir.join("ui-mode");
        Ok(fs::read_to_string(saved)
            .ok()
            .as_deref()
            .and_then(Self::parse)
            .unwrap_or_default())
    }

    pub fn set(self) -> Result<()> {
        let paths = Paths::discover()?;
        fs::create_dir_all(&paths.config_dir)?;
        let temporary = paths
            .config_dir
            .join(format!(".ui-mode-{}", std::process::id()));
        fs::write(&temporary, format!("{}\n", self.name()))?;
        fs::rename(&temporary, paths.config_dir.join("ui-mode"))?;
        if let Ok(server) = ServerIdentity::discover() {
            let status = Command::new("tmux")
                .args(server.tmux_args())
                .args(["set-option", "-g", "@workbench-ui", self.name()])
                .status()?;
            if !status.success() {
                return Err("could not change this tmux server's UI backend".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Choice {
    Old,
    New,
    Toggle,
    Status,
}

pub fn run(choice: Choice) -> Result<()> {
    let mode = match choice {
        Choice::Old => Mode::Old,
        Choice::New => Mode::New,
        Choice::Toggle if Mode::current()? == Mode::Old => Mode::New,
        Choice::Toggle => Mode::Old,
        Choice::Status => {
            println!("{}", Mode::current()?.name());
            return Ok(());
        }
    };
    mode.set()?;
    println!("{}", mode.name());
    Ok(())
}
