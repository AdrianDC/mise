use std::ffi::OsString;

use eyre::{Result, bail};

use super::{Cli, Commands, usage_error};

/// Pick several tasks to run from a menu
///
/// Opens a menu of tasks grouped by folder, the part of a task's name before
/// its last `:`. Press space to check a task, `a` to check or uncheck all, and
/// `/` to filter; checking a folder's line runs every task in it. Enter runs
/// the checked tasks as if they were named with `:::`, and Esc cancels.
///
/// `--run` runs the presets without opening the menu.
///
/// The same as `mise run --select`, which also takes `mise run` flags. Global
/// flags go before `select`, such as `mise -j 1 select` to run the tasks one at
/// a time.
#[derive(Debug, usage_rs::Args)]
#[usage(
    example(r###"mise select"###, help = "Pick tasks to run"),
    example(
        r###"mise select lint test"###,
        help = r###"Pick tasks, with "lint" and every task in the "test" folder checked"###
    ),
    example(
        r###"mise select --run build"###,
        help = r###"Run every task in the "build" folder, without the menu"###
    ),
    example(
        r###"mise -j 1 select"###,
        help = "Pick tasks and run them one at a time"
    )
)]
pub(crate) struct Select {
    /// Task names, aliases, or folders to check when the menu opens
    #[usage()]
    pub presets: Vec<String>,

    /// Show task aliases in the menu
    #[usage(long)]
    pub alias: bool,

    /// Run the presets without opening the menu
    #[usage(long)]
    pub run: bool,
}

impl Select {
    pub(crate) async fn run(self) -> Result<()> {
        // Parsed, not built, to keep `mise run`'s defaults and env vars.
        let alias = self.alias.then_some("--alias");
        let argv = ["mise", "run", "--select"]
            .into_iter()
            .chain(alias)
            .map(OsString::from)
            .chain(self.presets.into_iter().map(OsString::from))
            .collect::<Vec<_>>();
        let argv = argv.iter().map(OsString::as_os_str).collect::<Vec<_>>();
        let cli = Cli::parse_from_argv(&argv).map_err(|err| usage_error(&argv[1..], err))?;
        let Some(Commands::Run(mut run)) = cli.command else {
            bail!("mise select: failed to build the run command");
        };
        run.select_run = self.run;
        run.run().await
    }
}
