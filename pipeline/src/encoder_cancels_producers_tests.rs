use super::*;
use crate::error::EXIT_FRAME;
use std::io::Read;
use std::sync::mpsc;

#[test]
fn premature_encoder_exit_cancels_every_registered_producer() {
    for exit in [0, 7] {
        let root = std::env::var_os("RAWTL_TEST_WORK")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp")
            })
            .join(format!(
                "encoder-cancellation-{}-{exit}",
                std::process::id()
            ));
        fs::create_dir_all(&root).unwrap();
        let active: proc::ActiveNative = Default::default();
        struct Cleanup(proc::ActiveNative);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                proc::cancel(&self.0);
            }
        }
        let _cleanup = Cleanup(active.clone());
        let (done, results) = mpsc::channel();
        let (ready, readiness) = mpsc::channel();
        let mut workers = vec![];
        for id in 0..3 {
            let owner = active.clone();
            let ready = ready.clone();
            let done = done.clone();
            let log = root.join(format!("producer-{id}.log"));
            workers.push(std::thread::spawn(move || {
                let args = vec![
                    "-c".into(),
                    "import signal; print('R',flush=True); signal.pause()".into(),
                ];
                let mut process = proc::OwnedProcess::spawn_active(
                    Path::new("/usr/bin/python3"),
                    &args,
                    &log,
                    EXIT_FRAME,
                    false,
                    owner,
                )
                .unwrap();
                let pid = process.child.lock().unwrap().id();
                let mut stdout = process.child.lock().unwrap().stdout.take().unwrap();
                stdout.read_exact(&mut [0]).unwrap();
                ready.send(pid).unwrap();
                let status = process.finish(false).unwrap();
                done.send((pid, status)).unwrap();
            }));
        }
        for _ in 0..3 {
            readiness
                .recv_timeout(Duration::from_secs(5))
                .expect("producer readiness deadline");
        }
        assert_eq!(active.lock().unwrap().children.len(), 3);
        let args = vec!["-c".into(), format!("import sys; sys.exit({exit})")];
        let mut encoder = StreamingEncoder::spawn(
            Path::new("/usr/bin/python3"),
            &args,
            &root.join("encoder.log"),
            active.clone(),
        )
        .unwrap();
        for _ in 0..3 {
            let (pid, status) = results.recv_timeout(Duration::from_secs(8)).unwrap();
            assert!(!status.success());
            println!("encoder_exit={exit} producer_pid={pid} actual_status={status}");
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(encoder.failed.load(std::sync::atomic::Ordering::SeqCst));
        // The encoder now shares cancellation ownership; all producers have
        // unregistered, while its own registration lasts until finish/reap.
        assert_eq!(
            active
                .lock()
                .unwrap()
                .children
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec![root.join("encoder.log")]
        );
        assert_eq!(encoder.finish(false).unwrap().code(), Some(exit));
        assert!(active.lock().unwrap().children.is_empty());
    }
}

#[test]
fn encoder_may_exit_successfully_after_the_final_input_transfer() {
    let root = std::env::var_os("RAWTL_TEST_WORK")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp")
        });
    fs::create_dir_all(&root).unwrap();
    let active: proc::ActiveNative = Default::default();
    let args = vec![
        "-c".into(),
        "import sys; assert sys.stdin.buffer.read(4)==b'data'".into(),
    ];
    let mut encoder = StreamingEncoder::spawn(
        Path::new("/usr/bin/python3"),
        &args,
        &root.join("final-encoder-input.log"),
        active.clone(),
    )
    .unwrap();
    encoder
        .final_input
        .store(true, std::sync::atomic::Ordering::SeqCst);
    encoder.input.as_mut().unwrap().write_all(b"data").unwrap();
    // Joining the observer proves it saw the exit before intentional end().
    let observer = encoder.watcher.take().unwrap();
    let (done, finished) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        observer.join().unwrap();
        let _ = done.send(());
    });
    finished
        .recv_timeout(Duration::from_secs(5))
        .expect("encoder observer deadline");
    waiter.join().unwrap();
    assert!(!encoder.failed.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!active.lock().unwrap().cancelled);
    assert_eq!(encoder.finish(false).unwrap().code(), Some(0));
}
