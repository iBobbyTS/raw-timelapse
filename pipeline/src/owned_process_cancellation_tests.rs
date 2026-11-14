use super::*;
use std::io::Read;
use std::sync::mpsc;

#[test]
fn cancellation_reaps_every_registered_task_and_preserves_other_registrations() {
    let root = std::path::PathBuf::from(
        std::env::var_os("RAWTL_TEST_WORK")
            .unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/target/test-tmp").into()),
    )
    .join(format!("owned-processes-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let active: ActiveNative = Default::default();
    struct CancelOnDrop(ActiveNative);
    impl Drop for CancelOnDrop {
        fn drop(&mut self) {
            cancel(&self.0);
        }
    }
    let _cleanup = CancelOnDrop(active.clone());
    let (done, results) = mpsc::channel();
    let (ready, readiness) = mpsc::channel();
    let mut workers = vec![];
    for id in 0..3 {
        let active = active.clone();
        let log = root.join(format!("task-{id}.log"));
        let ready = ready.clone();
        let done = done.clone();
        workers.push(std::thread::spawn(move || {
            let ignore_term = if id == 2 {
                "signal.signal(signal.SIGTERM,signal.SIG_IGN); "
            } else {
                ""
            };
            let args = vec![
                "-c".into(),
                format!("import signal; {ignore_term}print('R',flush=True); signal.pause()"),
            ];
            let mut child = OwnedProcess::spawn_active(
                Path::new("/usr/bin/python3"),
                &args,
                &log,
                30,
                false,
                active,
            )
            .unwrap();
            let pid = child.child.lock().unwrap().id();
            let mut stdout = child.child.lock().unwrap().stdout.take().unwrap();
            stdout.read_exact(&mut [0]).unwrap();
            ready.send(pid).unwrap();
            let status = child.finish(false).unwrap();
            println!("task={id} pid={pid} actual_status={status}");
            if id == 2 {
                use std::os::unix::process::ExitStatusExt;
                assert_eq!(status.signal(), Some(9));
            }
            done.send((id, pid, status)).unwrap();
        }));
    }
    let pids: Vec<_> = (0..3)
        .map(|_| {
            readiness
                .recv_timeout(Duration::from_secs(5))
                .expect("producer readiness deadline")
        })
        .collect();
    assert_eq!(active.lock().unwrap().children.len(), 3);
    // Finishing one registration cannot erase the two other active tasks.
    let first = active
        .lock()
        .unwrap()
        .children
        .values()
        .next()
        .unwrap()
        .clone();
    terminate(&first);
    let (_, pid, status) = results.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(pids.contains(&pid) && !status.success());
    // Result is sent before the owner is dropped; finish itself must unregister.
    assert_eq!(active.lock().unwrap().children.len(), 2);
    cancel(&active);
    for _ in 0..2 {
        let (_, pid, status) = results.recv_timeout(Duration::from_secs(8)).unwrap();
        assert!(pids.contains(&pid) && !status.success());
        assert!(
            std::process::Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .status()
                .unwrap()
                .code()
                != Some(0)
        );
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert!(active.lock().unwrap().children.is_empty());
    let args = vec!["-c".into(), "exit 0".into()];
    assert!(OwnedProcess::spawn_active(
        Path::new("/bin/sh"),
        &args,
        &root.join("late.log"),
        30,
        false,
        active
    )
    .is_err());
}
