//! `suvadu`: the same command as `suv`, under the package's own name.
//!
//! Homebrew and the install script put a `suvadu` link beside `suv`;
//! `cargo install suvadu` installs this instead, so `suvadu mcp-serve` works
//! however Suvadu was installed. It runs the `suv` installed beside it with
//! the same arguments.
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let suv = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.parent()
                .map(|dir| dir.join(format!("suv{}", std::env::consts::EXE_SUFFIX)))
        })
        .filter(|path| path.is_file())
        .unwrap_or_else(|| "suv".into());
    let mut command = Command::new(&suv);
    command.args(std::env::args_os().skip(1));

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Replace this process, so signals, stdin/stdout and the exit status
        // are suv's own (the MCP server talks over stdio).
        let error = command.exec();
        eprintln!("suvadu: cannot run {}: {error}", suv.display());
        ExitCode::from(127)
    }
    #[cfg(not(unix))]
    {
        match command.status() {
            Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
            Err(error) => {
                eprintln!("suvadu: cannot run {}: {error}", suv.display());
                ExitCode::from(127)
            }
        }
    }
}
