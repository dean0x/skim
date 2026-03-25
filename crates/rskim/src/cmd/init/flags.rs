//! Flag parsing for `skim init`.

/// Parsed command-line flags for the init subcommand.
#[derive(Debug)]
pub(super) struct InitFlags {
    pub(super) project: bool,
    pub(super) yes: bool,
    pub(super) dry_run: bool,
    pub(super) uninstall: bool,
    pub(super) force: bool,
}

pub(super) fn parse_flags(args: &[String]) -> anyhow::Result<InitFlags> {
    let mut project = false;
    let mut yes = false;
    let mut dry_run = false;
    let mut uninstall = false;
    let mut force = false;

    for arg in args {
        match arg.as_str() {
            "--global" => { /* default, no-op */ }
            "--project" => project = true,
            "--yes" | "-y" => yes = true,
            "--dry-run" => dry_run = true,
            "--uninstall" => uninstall = true,
            "--force" => force = true,
            other => {
                anyhow::bail!(
                    "unknown flag: '{other}'\n\
                     Run 'skim init --help' for usage information"
                );
            }
        }
    }

    Ok(InitFlags {
        project,
        yes,
        dry_run,
        uninstall,
        force,
    })
}
