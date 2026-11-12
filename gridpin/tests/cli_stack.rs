//! The CLI must not depend on the size of the main thread's stack: Windows gives it 1 MiB, Linux and
//! macOS 8 MiB. v0.9.0 CI: a debug build overflowed 1 MiB in argument parsing alone and every CLI test
//! failed on Windows. Here the main thread is limited to the Windows size with `ulimit -s` (unix only;
//! on Windows the test suite itself runs under that limit).

#[cfg(unix)]
#[test]
fn the_cli_answers_with_the_one_mib_main_stack_of_windows() {
    let bin = env!("CARGO_BIN_EXE_gridpin");
    for args in ["--version", "build --help", "query --help"] {
        let out = std::process::Command::new("sh")
            .args(["-c", &format!("ulimit -s 1024 && exec \"$0\" {args}"), bin])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "`gridpin {args}` with a 1 MiB main stack: {:?}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
