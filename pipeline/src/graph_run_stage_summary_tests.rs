//! Exercise the real graph/execute/publication boundary with append I/O failure.
//! Each scenario runs in its own test process so SIGINT never touches peers.
use crate::{cli, proc, run_log::RunLog};
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Instant;

fn executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn scenario(mode: &str, expected: i32, cause: &str) {
    let name = format!("graph_run::stage_summary_tests::summary_append_failure_{mode}");
    // A unique no-match skip filter marks the isolated child without an
    // environment setting or changing normal libtest flag semantics.
    let marker = format!("__summary_append_child_{mode}__");
    let child = std::env::args().any(|arg| arg == marker);
    let root = std::env::var_os("RAWTL_TEST_WORK")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp"))
        .join(format!("summary-append-{mode}-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    if !child {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--skip", &marker, "--nocapture"])
            .output()
            .unwrap();
        fs::write(root.join("child.stdout"), &output.stdout).unwrap();
        fs::write(root.join("child.stderr"), &output.stderr).unwrap();
        fs::write(
            root.join("child-exit.txt"),
            format!("{:?}\n", output.status.code()),
        )
        .unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    proc::install_stop_handlers();
    let native = root.join("fake-native");
    let built = Command::new("rustc")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/common/fake_native_main.rs"))
        .args(["--edition=2021", "-o"])
        .arg(&native)
        .output()
        .unwrap();
    fs::write(root.join("producer-build.stdout"), &built.stdout).unwrap();
    fs::write(root.join("producer-build.stderr"), &built.stderr).unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let input = root.join("input");
    let scratch = root.join("scratch");
    fs::create_dir_all(&input).unwrap();
    fs::create_dir_all(&scratch).unwrap();
    fs::write(input.join("IMG_1.CR3"), b"controlled source").unwrap();
    let config = root.join("producer.config");
    fs::write(
        &config,
        format!(
            "width=8\nheight=6\ngrid_nx=4\ngrid_ny=2\nframe=IMG_1.CR3|0.5,1,0.75|0|0|-|-|flat\n{}",
            if mode == "frame" {
                "frame_fail_on=IMG_1.CR3\n"
            } else {
                ""
            }
        ),
    )
    .unwrap();
    let wrapper = root.join("native-adapter");
    executable(
        &wrapper,
        &format!(
            "#!/bin/sh\nexport FAKE_NATIVE_CONFIG='{}'\nexec '{}' \"$@\"\n",
            config.display(),
            native.display()
        ),
    );
    let encoder = root.join("encoder");
    executable(&encoder, &format!("#!/usr/bin/python3\nimport pathlib,sys\nif '-h' in sys.argv or 'lavfi' in sys.argv: sys.exit(0)\n{}\nsys.stdin.buffer.read()\npathlib.Path(sys.argv[-1]).write_bytes(b'controlled encoded output')\n",
        if mode == "encoder" { "sys.stderr.write('selected encoder failure\\n');sys.exit(9)" } else { "" }));
    let video = root.join("timelapse.mov");
    let log_path = root.join("timelapse.log");
    let args: Vec<String> = [
        "--input",
        input.to_str().unwrap(),
        "--output",
        video.to_str().unwrap(),
        "--strength",
        "0",
        "--dng-linear-frame",
        wrapper.to_str().unwrap(),
        "--ffmpeg",
        encoder.to_str().unwrap(),
        "--ffprobe",
        "/usr/bin/true",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let cli::Parsed::Run(options) = cli::parse(&args).unwrap() else {
        panic!("run options")
    };
    let mut log = RunLog::create(&log_path, "test").unwrap();
    let failed = Arc::new(AtomicBool::new(false));
    let attempted = failed.clone();
    let node_error = Arc::new(AtomicBool::new(false));
    let error_attempted = node_error.clone();
    let stop = mode == "stop";
    let read_path = log_path.clone();
    log.before_line(move |payload, file| {
        if payload.starts_with("kind=node.error ") {
            error_attempted.store(true, Ordering::SeqCst);
        }
        if stop && payload.starts_with("kind=node node=apply-ev ") {
            // Ordered commit establishes the upstream work before cancellation.
            unsafe extern "C" {
                fn kill(pid: i32, sig: i32) -> i32;
            }
            assert_eq!(unsafe { kill(std::process::id() as i32, 2) }, 0);
        }
        if payload.starts_with("kind=stage ") {
            // Switching to a read-only descriptor causes the real append to
            // fail (EBADF), independent of clock, disk pressure or sleeps.
            *file = File::open(&read_path).unwrap();
            attempted.store(true, Ordering::SeqCst);
        }
    });
    let error = crate::execute(&options, &input, &video, &scratch, Instant::now(), &mut log)
        .expect_err("summary failure or selected graph failure must prevent publication");
    assert!(
        failed.load(Ordering::SeqCst),
        "must reach the summary append"
    );
    assert_eq!(error.code, expected, "{}", error.message);
    assert!(error.message.contains(cause), "{}", error.message);
    assert_eq!(node_error.load(Ordering::SeqCst), mode != "success");
    if mode == "encoder" {
        assert!(
            error.message.contains("selected encoder failure"),
            "{}",
            error.message
        );
    }
    if mode == "frame" {
        assert!(
            error.message.contains("frame 1/1 (IMG_1.CR3) failed:"),
            "{}",
            error.message
        );
    }
    assert!(!video.exists(), "no publication");
    assert!(
        scratch.is_dir() && fs::read_dir(&scratch).unwrap().next().is_some(),
        "scratch retained"
    );
    let trace = fs::read_to_string(&log_path).unwrap();
    if mode == "success" {
        assert!(trace.contains("kind=node node=encode frame=1 "), "{trace}");
        assert!(
            scratch.join("timelapse.mov").is_file(),
            "graph finished encoding before summary failure"
        );
    }
    fs::write(root.join("outcome.txt"), format!("code={}\nmessage={}\nsummary_append_failed=true\nscratch_retained=true\npublished=false\n", error.code, error.message)).unwrap();
    eprintln!(
        "selected run outcome: code={} {}",
        error.code, error.message
    );
    std::process::exit(error.code);
}

#[test]
fn summary_append_failure_frame() {
    scenario("frame", 30, "synthetic mid-run frame failure");
}
#[test]
fn summary_append_failure_encoder() {
    scenario("encoder", 60, "streaming encoder failed");
}
#[test]
fn summary_append_failure_stop() {
    scenario("stop", 130, "run cancelled");
}
#[test]
fn summary_append_failure_success() {
    scenario("success", 70, "cannot write run log");
}
