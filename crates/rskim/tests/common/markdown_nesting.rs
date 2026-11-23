use std::process::Output;

pub fn nested_report(body: &str) -> String {
    format!("# Report\n\n{} {body}\n", ">".repeat(200))
}

pub fn clean(cmd: &mut assert_cmd::Command) -> &mut assert_cmd::Command {
    cmd.env_remove("SKIM_PASSTHROUGH").env_remove("SKIM_DEBUG")
}

pub fn run(cmd: &mut assert_cmd::Command) -> Output {
    let output = cmd.output().expect("run skim");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            output.status.signal(),
            None,
            "skim stopped: {:?}",
            output.status
        );
    }
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
